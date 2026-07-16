use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce, aead::AeadInOut};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::error::{Result, XferError};

const SESSION_CONTEXT: &[u8] = b"xfer-v4-session";
const SESSION_MATERIAL_LEN: usize = 72;

#[derive(Clone)]
pub struct DirectionalKey {
    key: [u8; 32],
    nonce_prefix: [u8; 4],
}

impl DirectionalKey {
    pub fn seal(&self, sequence: u64, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut ciphertext = Vec::with_capacity(plaintext.len() + 16);
        self.seal_into(sequence, aad, plaintext, &mut ciphertext)?;
        Ok(ciphertext)
    }

    pub fn seal_into(
        &self,
        sequence: u64,
        aad: &[u8],
        plaintext: &[u8],
        ciphertext: &mut Vec<u8>,
    ) -> Result<()> {
        let cipher = ChaCha20Poly1305::new(&Key::from(self.key));
        ciphertext.clear();
        ciphertext.reserve(plaintext.len() + 16);
        ciphertext.extend_from_slice(plaintext);
        cipher
            .encrypt_in_place(&self.nonce(sequence), aad, ciphertext)
            .map_err(|_| XferError::security("could not encrypt protocol record"))
    }

    pub fn open(&self, sequence: u64, aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
        let mut plaintext = ciphertext.to_vec();
        self.open_in_place(sequence, aad, &mut plaintext)?;
        Ok(plaintext)
    }

    pub fn open_in_place(&self, sequence: u64, aad: &[u8], ciphertext: &mut Vec<u8>) -> Result<()> {
        let cipher = ChaCha20Poly1305::new(&Key::from(self.key));
        cipher
            .decrypt_in_place(&self.nonce(sequence), aad, ciphertext)
            .map_err(|_| {
                XferError::security(
                    "record authentication failed; the key, token, or record contents differ",
                )
            })
    }

    fn nonce(&self, sequence: u64) -> Nonce {
        let mut bytes = [0_u8; 12];
        bytes[..4].copy_from_slice(&self.nonce_prefix);
        bytes[4..].copy_from_slice(&sequence.to_be_bytes());
        Nonce::from(bytes)
    }
}

#[derive(Clone)]
pub struct SessionKeys {
    pub client_to_server: DirectionalKey,
    pub server_to_client: DirectionalKey,
}

pub fn derive_session_keys(
    local_secret: &StaticSecret,
    remote_public: &PublicKey,
    server_public: &[u8; 32],
    client_public: &[u8; 32],
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    token: Option<&str>,
) -> Result<SessionKeys> {
    let shared = local_secret.diffie_hellman(remote_public);
    if !shared.was_contributory() {
        return Err(XferError::security("peer supplied an invalid public key"));
    }
    let mut salt_hasher = Sha256::new();
    salt_hasher.update(b"xfer-v4-salt");
    salt_hasher.update(server_public);
    salt_hasher.update(client_public);
    salt_hasher.update(server_nonce);
    salt_hasher.update(client_nonce);
    if let Some(token) = token {
        salt_hasher.update(token.as_bytes());
    }
    let salt = salt_hasher.finalize();
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut material = [0_u8; SESSION_MATERIAL_LEN];
    hkdf.expand(SESSION_CONTEXT, &mut material)
        .map_err(|_| XferError::security("could not derive session keys"))?;

    let mut client_key = [0_u8; 32];
    client_key.copy_from_slice(&material[..32]);
    let mut server_key = [0_u8; 32];
    server_key.copy_from_slice(&material[32..64]);
    let mut client_nonce = [0_u8; 4];
    client_nonce.copy_from_slice(&material[64..68]);
    let mut server_nonce = [0_u8; 4];
    server_nonce.copy_from_slice(&material[68..72]);

    Ok(SessionKeys {
        client_to_server: DirectionalKey {
            key: client_key,
            nonce_prefix: client_nonce,
        },
        server_to_client: DirectionalKey {
            key: server_key,
            nonce_prefix: server_nonce,
        },
    })
}

pub fn fingerprint(public_key: &[u8; 32]) -> String {
    let digest = Sha256::digest(public_key);
    hex::encode(digest)
}

pub fn display_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .as_bytes()
        .chunks(4)
        .take(8)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

pub fn sas(
    server_public: &[u8; 32],
    client_public: &[u8; 32],
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    token: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"xfer-v4-sas");
    hasher.update(server_public);
    hasher.update(client_public);
    hasher.update(server_nonce);
    hasher.update(client_nonce);
    if let Some(token) = token {
        hasher.update(token.as_bytes());
    }
    let digest = hasher.finalize();
    let value = u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]) % 10_000_000_000;
    let digits = format!("{value:010}");
    format!("{}-{}-{}", &digits[..3], &digits[3..6], &digits[6..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_roles_derive_the_same_keys() {
        let server = StaticSecret::from([3_u8; 32]);
        let client = StaticSecret::from([7_u8; 32]);
        let server_public = PublicKey::from(&server);
        let client_public = PublicKey::from(&client);
        let server_nonce = [1_u8; 32];
        let client_nonce = [2_u8; 32];

        let from_server = derive_session_keys(
            &server,
            &client_public,
            server_public.as_bytes(),
            client_public.as_bytes(),
            &server_nonce,
            &client_nonce,
            None,
        )
        .unwrap();
        let from_client = derive_session_keys(
            &client,
            &server_public,
            server_public.as_bytes(),
            client_public.as_bytes(),
            &server_nonce,
            &client_nonce,
            None,
        )
        .unwrap();

        let aad = b"header";
        let ciphertext = from_client
            .client_to_server
            .seal(0, aad, b"payload")
            .unwrap();
        assert_eq!(
            from_server
                .client_to_server
                .open(0, aad, &ciphertext)
                .unwrap(),
            b"payload"
        );
    }

    #[test]
    fn authenticated_encryption_rejects_tampering() {
        let key = DirectionalKey {
            key: [7_u8; 32],
            nonce_prefix: [9_u8; 4],
        };
        let mut ciphertext = key.seal(3, b"header", b"payload").unwrap();
        ciphertext[0] ^= 1;
        assert!(key.open(3, b"header", &ciphertext).is_err());
    }

    #[test]
    fn caller_owned_ciphertext_buffer_is_reused() {
        let key = DirectionalKey {
            key: [7_u8; 32],
            nonce_prefix: [9_u8; 4],
        };
        let mut ciphertext = Vec::new();
        let first = vec![5_u8; 1024 * 1024];
        key.seal_into(0, b"header", &first, &mut ciphertext)
            .unwrap();
        let capacity = ciphertext.capacity();
        key.open_in_place(0, b"header", &mut ciphertext).unwrap();
        assert_eq!(ciphertext, first);

        let second = vec![6_u8; 1024 * 1024];
        key.seal_into(1, b"header", &second, &mut ciphertext)
            .unwrap();
        assert_eq!(ciphertext.capacity(), capacity);
    }

    #[test]
    fn key_derivation_rejects_non_contributory_public_keys() {
        let secret = StaticSecret::from([3_u8; 32]);
        let invalid_public = PublicKey::from([0_u8; 32]);
        assert!(
            derive_session_keys(
                &secret,
                &invalid_public,
                &[1_u8; 32],
                &[0_u8; 32],
                &[2_u8; 32],
                &[3_u8; 32],
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn token_changes_session_keys_and_authentication() {
        let server = StaticSecret::from([3_u8; 32]);
        let client = StaticSecret::from([7_u8; 32]);
        let server_public = PublicKey::from(&server);
        let client_public = PublicKey::from(&client);
        let without_token = derive_session_keys(
            &client,
            &server_public,
            server_public.as_bytes(),
            client_public.as_bytes(),
            &[1_u8; 32],
            &[2_u8; 32],
            None,
        )
        .unwrap();
        let with_token = derive_session_keys(
            &client,
            &server_public,
            server_public.as_bytes(),
            client_public.as_bytes(),
            &[1_u8; 32],
            &[2_u8; 32],
            Some("secret"),
        )
        .unwrap();
        let ciphertext = with_token
            .client_to_server
            .seal(0, b"header", b"payload")
            .unwrap();
        assert!(
            without_token
                .client_to_server
                .open(0, b"header", &ciphertext)
                .is_err()
        );
    }

    #[test]
    fn sequence_and_associated_data_are_authenticated() {
        let key = DirectionalKey {
            key: [7_u8; 32],
            nonce_prefix: [9_u8; 4],
        };
        let ciphertext = key.seal(4, b"header", b"payload").unwrap();
        assert!(key.open(5, b"header", &ciphertext).is_err());
        assert!(key.open(4, b"different", &ciphertext).is_err());
    }

    #[test]
    fn fingerprint_and_sas_have_stable_display_shapes() {
        let fingerprint = fingerprint(&[3_u8; 32]);
        assert_eq!(fingerprint.len(), 64);
        let displayed = display_fingerprint(&fingerprint);
        assert_eq!(displayed.len(), 39);
        assert_eq!(displayed.matches(':').count(), 7);

        let sas = sas(&[1_u8; 32], &[2_u8; 32], &[3_u8; 32], &[4_u8; 32], None);
        assert_eq!(sas.len(), 12);
        assert_eq!(sas.as_bytes()[3], b'-');
        assert_eq!(sas.as_bytes()[7], b'-');
    }
}
