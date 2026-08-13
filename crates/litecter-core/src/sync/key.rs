//! The sync key — Litecter's entire identity story.
//!
//! There are no accounts. A device generates 32 random bytes on first sync and
//! that secret *is* the user: it addresses their document, authenticates the
//! request, and encrypts the payload. Linking a second machine means pasting
//! the same string. Losing it means losing the backup, which is the honest
//! trade for holding no email addresses and no plaintext watch lists.
//!
//! Three independent values are derived from the root secret, so possessing one
//! never yields another:
//!
//! ```text
//! root ──derive("…auth v1")───────► auth token  (bearer, sent to the server)
//!      ──derive("…encryption v1")─► cipher key  (never leaves the device)
//! ```
//!
//! The storage path is *not* derived here: the Worker hashes the bearer token
//! itself, so a client can only ever address the object it holds the key for.

use anyhow::{bail, Context, Result};

/// Crockford base32 — no I, L, O or U, so a key read aloud or copied by hand
/// can't turn into a different valid key.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

const KEY_BYTES: usize = 32;
const GROUP: usize = 4;

/// Domain separation strings. Changing one invalidates every existing key's
/// derived value, so they are versioned rather than edited.
const AUTH_CONTEXT: &str = "litecter sync auth v1";
const CIPHER_CONTEXT: &str = "litecter sync encryption v1";

#[derive(Clone, PartialEq, Eq)]
pub struct SyncKey([u8; KEY_BYTES]);

impl SyncKey {
    /// A fresh key from the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; KEY_BYTES];
        getrandom::fill(&mut bytes).context("reading from the system RNG")?;
        Ok(Self(bytes))
    }

    /// The bearer token sent to the sync endpoint, as lowercase hex. The server
    /// stores only its SHA-256, and this value reveals nothing about the
    /// cipher key.
    pub fn auth_token(&self) -> String {
        let derived = blake3::derive_key(AUTH_CONTEXT, &self.0);
        derived.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The symmetric key that seals the document body.
    pub fn cipher_key(&self) -> [u8; 32] {
        blake3::derive_key(CIPHER_CONTEXT, &self.0)
    }

    /// The user-facing form: Crockford base32 in dash-separated groups of four.
    pub fn encode(&self) -> String {
        let mut bits = 0u32;
        let mut nbits = 0u32;
        let mut chars = Vec::with_capacity(52);
        for &byte in &self.0 {
            bits = (bits << 8) | byte as u32;
            nbits += 8;
            while nbits >= 5 {
                nbits -= 5;
                chars.push(ALPHABET[((bits >> nbits) & 0x1f) as usize]);
            }
        }
        if nbits > 0 {
            chars.push(ALPHABET[((bits << (5 - nbits)) & 0x1f) as usize]);
        }
        chars
            .chunks(GROUP)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Parse a pasted key. Tolerant of case, dashes, and whitespace — a user
    /// moving this between machines will paste it with all three.
    pub fn decode(input: &str) -> Result<Self> {
        let mut bits = 0u32;
        let mut nbits = 0u32;
        let mut out = Vec::with_capacity(KEY_BYTES);
        for ch in input.chars() {
            if ch == '-' || ch.is_whitespace() {
                continue;
            }
            // Crockford's aliases: O reads as 0, I and L read as 1.
            let upper = match ch.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            let value = ALPHABET
                .iter()
                .position(|&a| a as char == upper)
                .with_context(|| format!("'{ch}' is not valid in a sync key"))?;
            bits = (bits << 5) | value as u32;
            nbits += 5;
            if nbits >= 8 {
                nbits -= 8;
                out.push(((bits >> nbits) & 0xff) as u8);
            }
        }
        if out.len() != KEY_BYTES {
            bail!(
                "a sync key is {KEY_BYTES} bytes ({} characters); got {}",
                KEY_BYTES * 8 / 5 + 1,
                out.len()
            );
        }
        let mut bytes = [0u8; KEY_BYTES];
        bytes.copy_from_slice(&out);
        Ok(Self(bytes))
    }
}

/// Never let a key reach a log line or an error message by accident.
impl std::fmt::Debug for SyncKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncKey(redacted)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_the_display_form() {
        let key = SyncKey::generate().unwrap();
        let encoded = key.encode();
        assert_eq!(SyncKey::decode(&encoded).unwrap(), key);
    }

    #[test]
    fn tolerates_how_people_actually_paste() {
        let key = SyncKey::generate().unwrap();
        let encoded = key.encode();
        for variant in [
            encoded.to_lowercase(),
            encoded.replace('-', ""),
            format!("  {encoded}\n"),
        ] {
            assert_eq!(SyncKey::decode(&variant).unwrap(), key, "variant: {variant}");
        }
    }

    #[test]
    fn rejects_garbage_rather_than_truncating() {
        assert!(SyncKey::decode("").is_err());
        assert!(SyncKey::decode("ABCD-EFGH").is_err(), "too short");
        assert!(SyncKey::decode(&"A".repeat(52)).is_ok());
        assert!(SyncKey::decode(&"A".repeat(60)).is_err(), "too long");
        assert!(SyncKey::decode("!!!!").is_err(), "invalid character");
    }

    #[test]
    fn derivations_are_independent() {
        let key = SyncKey::generate().unwrap();
        let token = key.auth_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        // The bearer token must not expose the key that decrypts the payload.
        assert_ne!(token.as_bytes(), &key.cipher_key()[..]);
        assert_ne!(key.cipher_key(), blake3::derive_key("litecter sync auth v1", &key.0));
    }

    #[test]
    fn distinct_keys_stay_distinct() {
        let a = SyncKey::generate().unwrap();
        let b = SyncKey::generate().unwrap();
        assert_ne!(a.auth_token(), b.auth_token());
        assert_ne!(a.cipher_key(), b.cipher_key());
    }
}
