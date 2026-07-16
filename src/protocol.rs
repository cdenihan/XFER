use std::io::{Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    crypto::DirectionalKey,
    error::{Result, XferError},
};

pub const VERSION: u16 = 4;
const VERSION_BYTE: u8 = 4;
pub const DEFAULT_PORT: u16 = 9_000;
pub const CHUNK_SIZE: usize = 1024 * 1024;
pub const MAX_RECORD_SIZE: usize = CHUNK_SIZE + 64 * 1024;

const NEGOTIATION_MAGIC: &[u8; 4] = b"XFR4";
const RECORD_MAGIC: &[u8; 4] = b"XR4R";
const HEADER_LEN: usize = 20;
const FLAG_SECURE: u8 = 1;
const RECORD_FLAG_ENCRYPTED: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameKind {
    Offer = 1,
    Decision = 2,
    EntryStart = 3,
    Data = 4,
    EntryEnd = 5,
    TransferEnd = 6,
    Complete = 7,
    Error = 8,
    Ready = 9,
}

impl TryFrom<u8> for FrameKind {
    type Error = XferError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Offer),
            2 => Ok(Self::Decision),
            3 => Ok(Self::EntryStart),
            4 => Ok(Self::Data),
            5 => Ok(Self::EntryEnd),
            6 => Ok(Self::TransferEnd),
            7 => Ok(Self::Complete),
            8 => Ok(Self::Error),
            9 => Ok(Self::Ready),
            other => Err(XferError::protocol(format!("unknown frame type {other}"))),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TransferKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Offer {
    pub root_name: String,
    pub kind: TransferKind,
    pub total_bytes: u64,
    pub file_count: u64,
    pub entry_count: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Decision {
    Accept,
    Reject(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EntryStart {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EntryEnd {
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TransferEnd {
    pub file_count: u64,
    pub total_bytes: u64,
    pub manifest_sha256: [u8; 32],
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Complete {
    pub destination: String,
    pub file_count: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ServerHello {
    pub public_key: [u8; 32],
    pub nonce: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct ClientHello {
    pub public_key: [u8; 32],
    pub nonce: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Client,
    Server,
}

pub struct RecordStream<S> {
    stream: S,
    send_key: Option<DirectionalKey>,
    receive_key: Option<DirectionalKey>,
    send_buffer: Vec<u8>,
    send_sequence: u64,
    receive_sequence: u64,
}

impl<S: Read + Write> RecordStream<S> {
    pub fn new(
        stream: S,
        role: Role,
        client_to_server: Option<DirectionalKey>,
        server_to_client: Option<DirectionalKey>,
    ) -> Self {
        let (send_key, receive_key) = match role {
            Role::Client => (client_to_server, server_to_client),
            Role::Server => (server_to_client, client_to_server),
        };
        Self {
            stream,
            send_key,
            receive_key,
            send_buffer: Vec::new(),
            send_sequence: 0,
            receive_sequence: 0,
        }
    }

    pub fn send_message<T: Serialize>(&mut self, kind: FrameKind, value: &T) -> Result<()> {
        let payload = serde_json::to_vec(value)
            .map_err(|error| XferError::Serialization(error.to_string()))?;
        self.send_frame(kind, &payload)
    }

    pub fn receive_message<T: DeserializeOwned>(&mut self, expected: FrameKind) -> Result<T> {
        let (kind, payload) = self.receive_frame()?;
        if kind == FrameKind::Error {
            let message: String = serde_json::from_slice(&payload)
                .map_err(|error| XferError::Serialization(error.to_string()))?;
            return Err(XferError::protocol(format!(
                "remote error: {}",
                sanitize_peer_text(&message)
            )));
        }
        if kind != expected {
            return Err(XferError::protocol(format!(
                "expected {expected:?}, received {kind:?}"
            )));
        }
        serde_json::from_slice(&payload)
            .map_err(|error| XferError::Serialization(error.to_string()))
    }

    pub fn send_frame(&mut self, kind: FrameKind, plaintext: &[u8]) -> Result<()> {
        if plaintext.len() > MAX_RECORD_SIZE {
            return Err(XferError::protocol(format!(
                "record is too large: {} bytes",
                plaintext.len()
            )));
        }

        let encrypted = self.send_key.is_some();
        let payload_len = plaintext.len() + usize::from(encrypted) * 16;
        let header = encode_header(
            kind,
            if encrypted { RECORD_FLAG_ENCRYPTED } else { 0 },
            self.send_sequence,
            payload_len,
        )?;
        if let Some(key) = &self.send_key {
            key.seal_into(
                self.send_sequence,
                &header,
                plaintext,
                &mut self.send_buffer,
            )?;
            self.stream.write_all(&header)?;
            self.stream.write_all(&self.send_buffer)?;
        } else {
            self.stream.write_all(&header)?;
            self.stream.write_all(plaintext)?;
        }
        self.stream.flush()?;
        self.send_sequence = self
            .send_sequence
            .checked_add(1)
            .ok_or_else(|| XferError::protocol("send sequence exhausted"))?;
        Ok(())
    }

    pub fn receive_frame(&mut self) -> Result<(FrameKind, Vec<u8>)> {
        let mut payload = Vec::new();
        let kind = self.receive_frame_into(&mut payload)?;
        Ok((kind, payload))
    }

    pub fn receive_frame_into(&mut self, payload: &mut Vec<u8>) -> Result<FrameKind> {
        let mut header = [0_u8; HEADER_LEN];
        self.stream.read_exact(&mut header)?;
        if &header[..4] != RECORD_MAGIC {
            return Err(XferError::protocol("invalid record magic"));
        }
        if header[4] != VERSION_BYTE {
            return Err(XferError::protocol(format!(
                "unsupported record version {}",
                header[4]
            )));
        }
        let kind = FrameKind::try_from(header[5])?;
        let flags = u16::from_be_bytes([header[6], header[7]]);
        if flags & !RECORD_FLAG_ENCRYPTED != 0 {
            return Err(XferError::protocol(format!(
                "record contains unsupported flags: {flags:#06x}"
            )));
        }
        let sequence = u64::from_be_bytes([
            header[8], header[9], header[10], header[11], header[12], header[13], header[14],
            header[15],
        ]);
        if sequence != self.receive_sequence {
            return Err(XferError::protocol(format!(
                "out-of-order record: expected {}, received {sequence}",
                self.receive_sequence
            )));
        }
        let length = u32::from_be_bytes([header[16], header[17], header[18], header[19]]) as usize;
        if length > MAX_RECORD_SIZE + 16 {
            return Err(XferError::protocol(format!(
                "incoming record is too large: {length} bytes"
            )));
        }
        let encrypted = flags & RECORD_FLAG_ENCRYPTED != 0;
        if encrypted != self.receive_key.is_some() {
            return Err(XferError::security(
                "record encryption state does not match the negotiated session",
            ));
        }

        payload.clear();
        payload.resize(length, 0);
        self.stream.read_exact(payload)?;
        if let Some(key) = &self.receive_key {
            key.open_in_place(sequence, &header, payload)?;
        }
        self.receive_sequence = self
            .receive_sequence
            .checked_add(1)
            .ok_or_else(|| XferError::protocol("receive sequence exhausted"))?;
        Ok(kind)
    }

    pub fn send_error(&mut self, message: &str) -> Result<()> {
        self.send_message(FrameKind::Error, &message.to_string())
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    pub fn into_inner(self) -> S {
        self.stream
    }
}

pub(crate) fn sanitize_peer_text(message: &str) -> String {
    let mut sanitized = String::with_capacity(message.len());
    for character in message.chars() {
        if character.is_control() {
            sanitized.extend(character.escape_default());
        } else {
            sanitized.push(character);
        }
    }
    sanitized
}

pub fn client_negotiate<S: Read + Write>(stream: &mut S, secure: bool) -> Result<()> {
    let mut preface = [0_u8; 8];
    preface[..4].copy_from_slice(NEGOTIATION_MAGIC);
    preface[4..6].copy_from_slice(&VERSION.to_be_bytes());
    preface[6] = u8::from(secure) * FLAG_SECURE;
    stream.write_all(&preface)?;
    stream.flush()?;

    let mut response = [0_u8; 8];
    stream.read_exact(&mut response)?;
    validate_negotiation_header(response)?;
    if response[6] != 0 {
        return Err(XferError::protocol(match response[6] {
            1 => "the receiver requires secure mode".into(),
            2 => "the receiver is in insecure mode".into(),
            3 => "the receiver does not support this protocol version".into(),
            code => format!("receiver rejected negotiation with code {code}"),
        }));
    }
    Ok(())
}

pub fn server_negotiate<S: Read + Write>(stream: &mut S, secure: bool) -> Result<()> {
    let mut preface = [0_u8; 8];
    stream.read_exact(&mut preface)?;
    validate_negotiation_header(preface)?;
    if preface[6] & !FLAG_SECURE != 0 {
        return Err(XferError::protocol(
            "sender used unsupported negotiation flags",
        ));
    }
    let client_secure = preface[6] & FLAG_SECURE != 0;
    let status = if client_secure == secure {
        0
    } else if secure {
        1
    } else {
        2
    };
    let mut response = [0_u8; 8];
    response[..4].copy_from_slice(NEGOTIATION_MAGIC);
    response[4..6].copy_from_slice(&VERSION.to_be_bytes());
    response[6] = status;
    stream.write_all(&response)?;
    stream.flush()?;
    if status != 0 {
        return Err(XferError::protocol(
            "sender and receiver security modes do not match",
        ));
    }
    Ok(())
}

pub fn write_server_hello<S: Write>(stream: &mut S, hello: &ServerHello) -> Result<()> {
    stream.write_all(&hello.public_key)?;
    stream.write_all(&hello.nonce)?;
    stream.flush()?;
    Ok(())
}

pub fn read_server_hello<S: Read>(stream: &mut S) -> Result<ServerHello> {
    let mut public_key = [0_u8; 32];
    let mut nonce = [0_u8; 32];
    stream.read_exact(&mut public_key)?;
    stream.read_exact(&mut nonce)?;
    Ok(ServerHello { public_key, nonce })
}

pub fn write_client_hello<S: Write>(stream: &mut S, hello: &ClientHello) -> Result<()> {
    stream.write_all(&hello.public_key)?;
    stream.write_all(&hello.nonce)?;
    stream.flush()?;
    Ok(())
}

pub fn read_client_hello<S: Read>(stream: &mut S) -> Result<ClientHello> {
    let mut public_key = [0_u8; 32];
    let mut nonce = [0_u8; 32];
    stream.read_exact(&mut public_key)?;
    stream.read_exact(&mut nonce)?;
    Ok(ClientHello { public_key, nonce })
}

fn validate_negotiation_header(header: [u8; 8]) -> Result<()> {
    if &header[..4] != NEGOTIATION_MAGIC {
        return Err(XferError::protocol("peer is not speaking XFER v4"));
    }
    let version = u16::from_be_bytes([header[4], header[5]]);
    if version != VERSION {
        return Err(XferError::protocol(format!(
            "protocol version mismatch: local {VERSION}, remote {version}"
        )));
    }
    if header[7] != 0 {
        return Err(XferError::protocol(
            "negotiation reserved byte must be zero",
        ));
    }
    Ok(())
}

fn encode_header(kind: FrameKind, flags: u16, sequence: u64, length: usize) -> Result<[u8; 20]> {
    let length = u32::try_from(length)
        .map_err(|_| XferError::protocol("record length does not fit in the wire format"))?;
    let mut header = [0_u8; HEADER_LEN];
    header[..4].copy_from_slice(RECORD_MAGIC);
    header[4] = VERSION_BYTE;
    header[5] = kind as u8;
    header[6..8].copy_from_slice(&flags.to_be_bytes());
    header[8..16].copy_from_slice(&sequence.to_be_bytes());
    header[16..20].copy_from_slice(&length.to_be_bytes());
    Ok(header)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn plaintext_records_round_trip() {
        let cursor = Cursor::new(Vec::new());
        let mut sender = RecordStream::new(cursor, Role::Client, None, None);
        sender.send_frame(FrameKind::Data, b"hello").unwrap();
        let cursor = Cursor::new(sender.into_inner().into_inner());
        let mut receiver = RecordStream::new(cursor, Role::Server, None, None);
        assert_eq!(
            receiver.receive_frame().unwrap(),
            (FrameKind::Data, b"hello".to_vec())
        );
    }

    #[test]
    fn oversized_record_is_rejected() {
        let cursor = Cursor::new(Vec::new());
        let mut stream = RecordStream::new(cursor, Role::Client, None, None);
        assert!(
            stream
                .send_frame(FrameKind::Data, &vec![0_u8; MAX_RECORD_SIZE + 1])
                .is_err()
        );
    }

    #[test]
    fn out_of_order_record_is_rejected() {
        let cursor = Cursor::new(Vec::new());
        let mut sender = RecordStream::new(cursor, Role::Client, None, None);
        sender.send_frame(FrameKind::Data, b"hello").unwrap();
        let mut bytes = sender.into_inner().into_inner();
        bytes[8..16].copy_from_slice(&1_u64.to_be_bytes());

        let mut receiver = RecordStream::new(Cursor::new(bytes), Role::Server, None, None);
        assert!(receiver.receive_frame().is_err());
    }

    #[test]
    fn unsupported_record_flags_are_rejected() {
        let cursor = Cursor::new(Vec::new());
        let mut sender = RecordStream::new(cursor, Role::Client, None, None);
        sender.send_frame(FrameKind::Data, b"hello").unwrap();
        let mut bytes = sender.into_inner().into_inner();
        bytes[7] = 0x02;

        let mut receiver = RecordStream::new(Cursor::new(bytes), Role::Server, None, None);
        assert!(receiver.receive_frame().is_err());
    }

    #[test]
    fn encrypted_flag_without_session_key_is_rejected() {
        let cursor = Cursor::new(Vec::new());
        let mut sender = RecordStream::new(cursor, Role::Client, None, None);
        sender.send_frame(FrameKind::Data, b"hello").unwrap();
        let mut bytes = sender.into_inner().into_inner();
        bytes[6..8].copy_from_slice(&RECORD_FLAG_ENCRYPTED.to_be_bytes());

        let mut receiver = RecordStream::new(Cursor::new(bytes), Role::Server, None, None);
        assert!(receiver.receive_frame().is_err());
    }

    #[test]
    fn remote_error_frame_is_returned_as_an_error() {
        let cursor = Cursor::new(Vec::new());
        let mut sender = RecordStream::new(cursor, Role::Client, None, None);
        sender.send_error("receiver rejected the offer").unwrap();

        let mut receiver = RecordStream::new(
            Cursor::new(sender.into_inner().into_inner()),
            Role::Server,
            None,
            None,
        );
        let error = receiver
            .receive_message::<Offer>(FrameKind::Offer)
            .unwrap_err()
            .to_string();
        assert!(error.contains("receiver rejected the offer"));
    }

    #[test]
    fn remote_error_control_characters_are_escaped() {
        let cursor = Cursor::new(Vec::new());
        let mut sender = RecordStream::new(cursor, Role::Client, None, None);
        sender.send_error("bad\u{1b}[2J\nmessage").unwrap();

        let mut receiver = RecordStream::new(
            Cursor::new(sender.into_inner().into_inner()),
            Role::Server,
            None,
            None,
        );
        let error = receiver
            .receive_message::<Offer>(FrameKind::Offer)
            .unwrap_err()
            .to_string();
        assert!(!error.contains('\u{1b}'));
        assert!(!error.contains('\n'));
        assert!(error.contains(r"\u{1b}[2J\nmessage"));
    }

    #[test]
    fn receive_frame_into_reuses_payload_capacity() {
        let cursor = Cursor::new(Vec::new());
        let mut sender = RecordStream::new(cursor, Role::Client, None, None);
        sender
            .send_frame(FrameKind::Data, &vec![1_u8; CHUNK_SIZE])
            .unwrap();
        sender
            .send_frame(FrameKind::Data, &vec![2_u8; CHUNK_SIZE])
            .unwrap();

        let mut receiver = RecordStream::new(
            Cursor::new(sender.into_inner().into_inner()),
            Role::Server,
            None,
            None,
        );
        let mut payload = Vec::new();
        receiver.receive_frame_into(&mut payload).unwrap();
        let capacity = payload.capacity();
        receiver.receive_frame_into(&mut payload).unwrap();
        assert_eq!(payload.capacity(), capacity);
        assert!(payload.iter().all(|byte| *byte == 2));
    }

    #[test]
    fn negotiation_rejects_wrong_magic_version_and_reserved_byte() {
        let mut wrong_magic = [0_u8; 8];
        wrong_magic[..4].copy_from_slice(b"NOPE");
        wrong_magic[4..6].copy_from_slice(&VERSION.to_be_bytes());
        assert!(validate_negotiation_header(wrong_magic).is_err());

        let mut wrong_version = [0_u8; 8];
        wrong_version[..4].copy_from_slice(NEGOTIATION_MAGIC);
        wrong_version[4..6].copy_from_slice(&(VERSION + 1).to_be_bytes());
        assert!(validate_negotiation_header(wrong_version).is_err());

        let mut reserved = [0_u8; 8];
        reserved[..4].copy_from_slice(NEGOTIATION_MAGIC);
        reserved[4..6].copy_from_slice(&VERSION.to_be_bytes());
        reserved[7] = 1;
        assert!(validate_negotiation_header(reserved).is_err());
    }

    #[test]
    fn server_rejects_security_mode_mismatch_and_unknown_flags() {
        let mut secure_preface = [0_u8; 8];
        secure_preface[..4].copy_from_slice(NEGOTIATION_MAGIC);
        secure_preface[4..6].copy_from_slice(&VERSION.to_be_bytes());
        secure_preface[6] = FLAG_SECURE;
        assert!(server_negotiate(&mut Cursor::new(secure_preface), false).is_err());

        let mut unknown_flags = secure_preface;
        unknown_flags[6] = 0x80;
        assert!(server_negotiate(&mut Cursor::new(unknown_flags), false).is_err());
    }
}
