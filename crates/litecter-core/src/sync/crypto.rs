//! Sealing a sync document for storage the server cannot read.
//!
//! Wire format:
//!
//! ```text
//! ┌────────┬─────────┬────────────┬──────────────────────────────┐
//! │ "LTCS" │ version │ nonce (24) │ XChaCha20-Poly1305 ciphertext │
//! └────────┴─────────┴────────────┴──────────────────────────────┘
//! ```
//!
//! The plaintext is zstd-compressed JSON. Compressing before encrypting is safe
//! here because the payload is a single user's own document — there is no
//! attacker-chosen content mixed in with a secret, so the compression-oracle
//! problem (CRIME/BREACH) doesn't apply. It matters a lot for size: page text
//! compresses ~5×, and this is the difference between a 1 MB and a 200 KB
//! upload.
//!
//! The nonce is random per seal rather than a counter. At 24 bytes the
//! collision probability is negligible, and a counter would need durable
//! state that survives a restore — exactly the thing that can't be relied on.

use anyhow::{bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{AeadCore, XChaCha20Poly1305, XNonce};

const MAGIC: &[u8; 4] = b"LTCS";
const VERSION: u8 = 1;
const NONCE_BYTES: usize = 24;
const HEADER_BYTES: usize = MAGIC.len() + 1 + NONCE_BYTES;

/// Refuse to inflate a hostile or corrupt payload into unbounded memory. Real
/// documents are ~100 KB; this is four orders of magnitude of headroom.
const MAX_PLAINTEXT_BYTES: usize = 64 * 1024 * 1024;

pub fn seal(cipher_key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let compressed = zstd::encode_all(plaintext, 3).context("compressing sync document")?;
    let cipher = XChaCha20Poly1305::new(cipher_key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, compressed.as_slice())
        .map_err(|_| anyhow::anyhow!("sealing sync document"))?;

    let mut out = Vec::with_capacity(HEADER_BYTES + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn open(cipher_key: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() <= HEADER_BYTES {
        bail!("sync document is truncated");
    }
    if &sealed[..4] != MAGIC {
        bail!("not a Litecter sync document");
    }
    let version = sealed[4];
    if version != VERSION {
        bail!("sync document is version {version}; this build understands {VERSION}. Update Litecter.");
    }

    let nonce = XNonce::from_slice(&sealed[5..5 + NONCE_BYTES]);
    let cipher = XChaCha20Poly1305::new(cipher_key.into());
    let compressed = cipher.decrypt(nonce, &sealed[HEADER_BYTES..]).map_err(|_| {
        anyhow::anyhow!("could not decrypt the sync document — is this the right sync key?")
    })?;

    zstd::decode_all(&compressed[..])
        .context("decompressing sync document")
        .and_then(|plain| {
            if plain.len() > MAX_PLAINTEXT_BYTES {
                bail!("sync document is implausibly large ({} bytes)", plain.len());
            }
            Ok(plain)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::key::SyncKey;

    #[test]
    fn roundtrips() {
        let key = SyncKey::generate().unwrap().cipher_key();
        let message = br#"{"v":1,"urls":[]}"#;
        assert_eq!(open(&key, &seal(&key, message).unwrap()).unwrap(), message);
    }

    #[test]
    fn the_wrong_key_cannot_open_it() {
        let a = SyncKey::generate().unwrap().cipher_key();
        let b = SyncKey::generate().unwrap().cipher_key();
        let sealed = seal(&a, b"secret watch list").unwrap();
        let err = open(&b, &sealed).unwrap_err().to_string();
        assert!(err.contains("right sync key"), "unhelpful error: {err}");
    }

    #[test]
    fn tampering_is_detected() {
        let key = SyncKey::generate().unwrap().cipher_key();
        let mut sealed = seal(&key, b"secret watch list").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(open(&key, &sealed).is_err(), "AEAD tag must reject a flipped bit");
    }

    #[test]
    fn rejects_foreign_and_truncated_input() {
        let key = SyncKey::generate().unwrap().cipher_key();
        assert!(open(&key, b"").is_err());
        assert!(open(&key, b"not-a-litecter-document-but-long-enough-to-pass-the-length-check").is_err());
        let sealed = seal(&key, b"hello").unwrap();
        assert!(open(&key, &sealed[..HEADER_BYTES]).is_err(), "truncated");
    }

    #[test]
    fn nonce_is_fresh_per_seal() {
        let key = SyncKey::generate().unwrap().cipher_key();
        // Identical plaintext must not produce identical bytes, or an observer
        // could tell that nothing changed between two uploads.
        assert_ne!(seal(&key, b"same").unwrap(), seal(&key, b"same").unwrap());
    }

    #[test]
    fn compression_actually_earns_its_place() {
        let key = SyncKey::generate().unwrap().cipher_key();
        let repetitive = "the page text repeats a great deal\n".repeat(500);
        let sealed = seal(&key, repetitive.as_bytes()).unwrap();
        assert!(
            sealed.len() < repetitive.len() / 4,
            "expected real compression, got {} from {}",
            sealed.len(),
            repetitive.len()
        );
    }
}
