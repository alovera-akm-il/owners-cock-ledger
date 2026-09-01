//! High-entropy opaque token generation + SHA-256 hashing, shared by
//! every "shown once, only the hash stored" credential in this schema:
//! invite tokens now, API tokens/password-reset tokens/2FA recovery
//! codes later (05-security-and-privacy.md §2, §9) — deliberately not
//! Argon2: these are already-high-entropy CSPRNG values, not
//! human-chosen secrets, so a fast hash is the right (and faster, at
//! verification time) choice.

use sha2::{Digest, Sha256};

/// A fresh high-entropy opaque token (two concatenated v4 UUIDs, each
/// backed by the OS CSPRNG — 256 bits total).
pub fn generate() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub fn hash(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_distinct_tokens() {
        assert_ne!(generate(), generate());
    }

    #[test]
    fn hash_is_deterministic_and_hex_encoded() {
        let raw = "fixed-value-for-this-test";
        let hashed = hash(raw);
        assert_eq!(hashed, hash(raw));
        assert_eq!(hashed.len(), 64);
        assert!(hashed.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn different_inputs_hash_differently() {
        assert_ne!(hash("a"), hash("b"));
    }
}
