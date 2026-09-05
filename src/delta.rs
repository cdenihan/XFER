//! Bounded-memory rsync-style matching: rolling weak sums plus SHA-256.
//! Algorithm: <https://rsync.samba.org/tech_report/node2.html>
use crate::{
    control::TransferControl,
    error::{Result, XferError},
    protocol::CHUNK_SIZE,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{BufReader, Read},
};

pub const MAX_BLOCKS: usize = 65_536;
pub const MIN_BLOCK: usize = 64 * 1024;
pub const MAX_BLOCK: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Signature {
    pub weak: u32,
    pub strong: [u8; 32],
    pub length: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BasisHeader {
    pub size: u64,
    pub block_size: u32,
    pub count: usize,
    pub unchanged: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SyncStats {
    pub sent_bytes: u64,
    pub reused_bytes: u64,
    pub changed_files: u64,
    pub unchanged_files: u64,
}

pub enum Instruction<'a> {
    Literal(&'a [u8]),
    Reuse(u32),
}

#[derive(Clone, Copy)]
struct Rolling {
    a: u32,
    b: u32,
}
impl Rolling {
    fn new(bytes: &[u8]) -> Self {
        let mut sum = Self { a: 0, b: 0 };
        for &byte in bytes {
            sum.a = sum.a.wrapping_add(u32::from(byte)) & 0xffff;
            sum.b = sum.b.wrapping_add(sum.a) & 0xffff;
        }
        sum
    }
    fn value(self) -> u32 {
        self.a | (self.b << 16)
    }
    fn roll(&mut self, old: u8, new: u8, length: usize) {
        self.a = self
            .a
            .wrapping_sub(u32::from(old))
            .wrapping_add(u32::from(new))
            & 0xffff;
        self.b = self
            .b
            .wrapping_sub(
                (u32::try_from(length).expect("bounded block length")).wrapping_mul(u32::from(old)),
            )
            .wrapping_add(self.a)
            & 0xffff;
    }
}

pub fn block_size(size: u64) -> usize {
    usize::try_from(size.div_ceil(MAX_BLOCKS as u64))
        .unwrap_or(MAX_BLOCK)
        .clamp(MIN_BLOCK, MAX_BLOCK)
        .next_power_of_two()
}

pub fn signatures(
    reader: &mut impl Read,
    size: u64,
    control: &TransferControl,
) -> Result<(BasisHeader, Vec<Signature>, [u8; 32])> {
    let block = block_size(size);
    let enabled = size.div_ceil(block as u64) <= MAX_BLOCKS as u64;
    let mut entries = Vec::new();
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    loop {
        control.check()?;
        let mut buffer = Vec::with_capacity(block);
        reader.take(block as u64).read_to_end(&mut buffer)?;
        if buffer.is_empty() {
            break;
        }
        bytes += buffer.len() as u64;
        if bytes > size {
            return Err(XferError::invalid_input("basis file grew while comparing"));
        }
        hash.update(&buffer);
        if enabled {
            entries.push(Signature {
                weak: Rolling::new(&buffer).value(),
                strong: Sha256::digest(&buffer).into(),
                length: u32::try_from(buffer.len()).expect("bounded block length"),
            });
        }
    }
    if bytes != size {
        return Err(XferError::invalid_input(
            "basis file changed while comparing",
        ));
    }
    Ok((
        BasisHeader {
            size,
            block_size: u32::try_from(block).expect("bounded block length"),
            count: entries.len(),
            unchanged: false,
        },
        entries,
        hash.finalize().into(),
    ))
}

pub fn validate_basis(header: &BasisHeader, blocks: &[Signature]) -> Result<()> {
    let block = header.block_size as usize;
    if !(MIN_BLOCK..=MAX_BLOCK).contains(&block)
        || !block.is_power_of_two()
        || header.count > MAX_BLOCKS
        || blocks.len() != header.count
    {
        return Err(XferError::protocol("invalid sync block signatures"));
    }
    if blocks.is_empty() {
        return Ok(());
    }
    if header.size.div_ceil(block as u64) != blocks.len() as u64 {
        return Err(XferError::protocol(
            "basis signature count does not match size",
        ));
    }
    for (index, signature) in blocks.iter().enumerate() {
        let expected = (header.size - index as u64 * block as u64).min(block as u64);
        if u64::from(signature.length) != expected {
            return Err(XferError::protocol("invalid basis block length"));
        }
    }
    Ok(())
}

pub fn encode(
    reader: impl Read,
    header: &BasisHeader,
    signatures: &[Signature],
    control: &TransferControl,
    mut emit: impl FnMut(Instruction<'_>) -> Result<()>,
) -> Result<(SyncStats, [u8; 32])> {
    validate_basis(header, signatures)?;
    let mut stats = SyncStats::default();
    let mut hash = Sha256::new();
    let mut reader = BufReader::with_capacity(CHUNK_SIZE, reader);
    if signatures.is_empty() {
        let mut buffer = vec![0; CHUNK_SIZE];
        loop {
            control.check()?;
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            emit(Instruction::Literal(&buffer[..count]))?;
            hash.update(&buffer[..count]);
            stats.sent_bytes += count as u64;
        }
        return Ok((stats, hash.finalize().into()));
    }
    let mut index = HashMap::<(u32, u32), HashMap<[u8; 32], u32>>::new();
    for (number, signature) in signatures.iter().enumerate() {
        index
            .entry((signature.length, signature.weak))
            .or_default()
            .entry(signature.strong)
            .or_insert(u32::try_from(number).expect("bounded signature count"));
    }
    let block = header.block_size as usize;
    let mut window = Vec::with_capacity(block);
    let mut literals = Vec::with_capacity(CHUNK_SIZE);
    let mut head = 0;
    let mut rolling = Rolling::new(&[]);
    loop {
        control.check()?;
        if window.is_empty() {
            reader
                .by_ref()
                .take(block as u64)
                .read_to_end(&mut window)?;
            if window.is_empty() {
                break;
            }
            head = 0;
            rolling = Rolling::new(&window);
        }
        let matched = index
            .get(&(
                u32::try_from(window.len()).expect("bounded window length"),
                rolling.value(),
            ))
            .and_then(|candidates| {
                let mut digest = Sha256::new();
                digest.update(&window[head..]);
                digest.update(&window[..head]);
                candidates
                    .get(&<[u8; 32]>::from(digest.finalize()))
                    .copied()
            });
        if let Some(number) = matched {
            if !literals.is_empty() {
                hash.update(&literals);
                emit(Instruction::Literal(&literals))?;
                literals.clear();
            }
            emit(Instruction::Reuse(number))?;
            hash.update(&window[head..]);
            hash.update(&window[..head]);
            stats.reused_bytes += window.len() as u64;
            window.clear();
        } else {
            let old = window[head];
            literals.push(old);
            stats.sent_bytes += 1;
            if literals.len() == CHUNK_SIZE {
                hash.update(&literals);
                emit(Instruction::Literal(&literals))?;
                literals.clear();
            }
            let mut next = [0];
            if reader.read(&mut next)? == 0 {
                // At EOF there are no further full-size matches to search for.
                for offset in 1..window.len() {
                    let byte = window[(head + offset) % window.len()];
                    literals.push(byte);
                    stats.sent_bytes += 1;
                    if literals.len() == CHUNK_SIZE {
                        hash.update(&literals);
                        emit(Instruction::Literal(&literals))?;
                        literals.clear();
                    }
                }
                window.clear();
            } else {
                window[head] = next[0];
                head = (head + 1) % window.len();
                rolling.roll(old, next[0], window.len());
            }
            if literals.len() >= CHUNK_SIZE {
                hash.update(&literals);
                emit(Instruction::Literal(&literals))?;
                literals.clear();
            }
        }
    }
    if !literals.is_empty() {
        hash.update(&literals);
        emit(Instruction::Literal(&literals))?;
    }
    Ok((stats, hash.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rolling_checksum_matches_recalculation() {
        let bytes = b"a sample with shifting bytes";
        let mut rolling = Rolling::new(&bytes[..7]);
        for offset in 1..bytes.len() - 7 {
            rolling.roll(bytes[offset - 1], bytes[offset + 6], 7);
            assert_eq!(
                rolling.value(),
                Rolling::new(&bytes[offset..offset + 7]).value()
            );
        }
    }
    #[test]
    fn inserted_prefix_reuses_shifted_blocks() {
        let basis = (0..MIN_BLOCK * 4)
            .map(|n| u8::try_from((n * 31 + n / 113) % 251).unwrap())
            .collect::<Vec<_>>();
        let mut source = b"inserted prefix".to_vec();
        source.extend_from_slice(&basis);
        let control = TransferControl::default();
        let (header, blocks, _) =
            signatures(&mut basis.as_slice(), basis.len() as u64, &control).unwrap();
        let mut result = Vec::new();
        let (stats, hash) = encode(
            source.as_slice(),
            &header,
            &blocks,
            &control,
            |instruction| {
                match instruction {
                    Instruction::Literal(bytes) => result.extend_from_slice(bytes),
                    Instruction::Reuse(index) => {
                        let start = index as usize * header.block_size as usize;
                        result.extend_from_slice(
                            &basis[start..start + blocks[index as usize].length as usize],
                        );
                    }
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result, source);
        assert_eq!(stats.sent_bytes, 15);
        assert_eq!(stats.reused_bytes, basis.len() as u64);
        assert_eq!(hash, <[u8; 32]>::from(Sha256::digest(&source)));
    }
    #[test]
    fn delta_round_trips_insertions_deletions_truncation_and_literal_boundaries() {
        let basis = (0..MIN_BLOCK * 2 + 113)
            .map(|n| u8::try_from((n * 17 + n / 97) % 251).unwrap())
            .collect::<Vec<_>>();
        let control = TransferControl::default();
        let (header, blocks, _) =
            signatures(&mut basis.as_slice(), basis.len() as u64, &control).unwrap();
        let mut inserted = basis.clone();
        inserted.splice(MIN_BLOCK / 2..MIN_BLOCK / 2, b"insertion".iter().copied());
        let mut deleted = basis.clone();
        deleted.drain(100..117);
        let cases = vec![
            Vec::new(),
            vec![4],
            basis[..57].to_vec(),
            basis.clone(),
            inserted,
            deleted,
            vec![255; CHUNK_SIZE * 2 + 19],
        ];
        for source in cases {
            let mut rebuilt = Vec::new();
            let (stats, digest) = encode(
                source.as_slice(),
                &header,
                &blocks,
                &control,
                |instruction| {
                    match instruction {
                        Instruction::Literal(bytes) => {
                            assert!(bytes.len() <= CHUNK_SIZE);
                            rebuilt.extend_from_slice(bytes);
                        }
                        Instruction::Reuse(index) => {
                            let start = index as usize * header.block_size as usize;
                            rebuilt.extend_from_slice(
                                &basis[start..start + blocks[index as usize].length as usize],
                            );
                        }
                    }
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(rebuilt, source);
            assert_eq!(stats.sent_bytes + stats.reused_bytes, source.len() as u64);
            assert_eq!(digest, <[u8; 32]>::from(Sha256::digest(&source)));
        }
    }
}
