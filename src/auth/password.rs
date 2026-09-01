//! Argon2id password hashing (07-tech-stack.md §1,
//! 05-security-and-privacy.md §2). Phase 1's login/invite-redemption
//! endpoints are this module's first real caller — not used yet outside
//! its own tests.
#![allow(dead_code)]

use argon2::password_hash::phc::PasswordHash;
use argon2::{Argon2, PasswordHasher, PasswordVerifier};

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let hash = Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to hash password: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A fixed Argon2id hash of a value nobody will ever type, computed once
/// and reused — never the hash of a real password. `POST /auth/login`
/// verifies against this when the email doesn't exist at all, so the
/// response timing for "no such account" and "wrong password" stays
/// indistinguishable (05-security-and-privacy.md §2).
pub fn dummy_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        hash_password("no-account-has-this-as-a-real-password-ever")
            .expect("dummy hash must always succeed")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_round_trips() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash));
    }

    #[test]
    fn wrong_password_fails() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(!verify_password("wrong password", &hash));
    }

    #[test]
    fn malformed_hash_fails_closed_rather_than_panicking() {
        assert!(!verify_password("anything", "not-a-real-phc-hash"));
    }

    #[test]
    fn dummy_hash_is_stable_and_verifiable_against_itself() {
        let hash = dummy_hash();
        assert!(verify_password(
            "no-account-has-this-as-a-real-password-ever",
            hash
        ));
        assert_eq!(hash, dummy_hash());
    }
}
