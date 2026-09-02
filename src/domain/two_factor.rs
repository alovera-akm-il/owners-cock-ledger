//! TOTP second factor (01-data-model.md §2, 10-operations.md §2,
//! 03-api-design.md §1). Setup is a two-step commit: `setup` writes a
//! pending, unconfirmed credential; only `confirm` — which proves the
//! user actually captured the secret by entering a real code back — sets
//! `confirmed_at` and starts enforcing it at login. Recovery codes exist
//! for the "lost the device on the very next login" failure mode; if
//! those are also exhausted, `admin disable-2fa` (10-operations.md §5,
//! `force_disable` below) is the last resort.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use totp_rs::{Algorithm, Builder, Secret, Totp};

use crate::auth::{session, token};

const ISSUER: &str = "Owner's Cock Ledger";
const RECOVERY_CODE_COUNT: usize = 10;

fn build_totp(secret_base32: &str, account_name: &str) -> anyhow::Result<Totp> {
    let secret = Secret::try_from_base32(secret_base32).map_err(|e| anyhow::anyhow!("{e}"))?;
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(6)
        .with_skew(1)
        .with_step_duration(30)
        .with_secret(secret)
        .with_issuer(Some(ISSUER))
        .with_account_name(account_name)
        .build()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub struct PendingSetup {
    pub secret_base32: String,
    pub otpauth_uri: String,
    /// Base64-encoded PNG, ready for `<img src="data:image/png;base64,...">`
    /// — a server-rendered QR code so the web UI needs no vendored JS
    /// scanning library (this deployment vendors every frontend asset
    /// locally rather than pulling from a CDN).
    pub qr_png_base64: String,
}

/// `POST /auth/2fa/setup` — generates a fresh secret and stores it with
/// `confirmed_at = NULL`. Calling this again before confirming replaces
/// the pending secret rather than erroring (`INSERT OR REPLACE`).
pub fn setup(conn: &Connection, user_id: &str, account_name: &str) -> anyhow::Result<PendingSetup> {
    let secret = Secret::generate();
    let secret_base32 = secret.to_base32();
    let totp = build_totp(&secret_base32, account_name)?;
    let otpauth_uri = totp.to_url()?;
    let qr_png_base64 = totp.to_qr_base64().map_err(|e| anyhow::anyhow!("{e}"))?;

    conn.execute(
        "INSERT INTO two_factor_credentials (user_id, secret, confirmed_at, created_at)
         VALUES (?1, ?2, NULL, ?3)
         ON CONFLICT(user_id) DO UPDATE SET secret = excluded.secret, confirmed_at = NULL, created_at = excluded.created_at
         WHERE two_factor_credentials.confirmed_at IS NULL",
        params![user_id, secret_base32, session::now()],
    )?;

    Ok(PendingSetup {
        secret_base32,
        otpauth_uri,
        qr_png_base64,
    })
}

#[derive(Debug, Error)]
pub enum ConfirmError {
    #[error("2FA is already enabled, or there's no pending setup to confirm")]
    NoPendingSetup,
    #[error("that code didn't match — check the time on your authenticator app")]
    InvalidCode,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// `POST /auth/2fa/confirm` — validates a real code against the pending
/// secret, then generates 10 recovery codes, returned raw exactly once
/// (05-security-and-privacy.md §2); only their hashes are stored.
pub fn confirm(
    conn: &mut Connection,
    user_id: &str,
    account_name: &str,
    code: &str,
) -> Result<Vec<String>, ConfirmError> {
    let secret_base32: Option<String> = conn
        .query_row(
            "SELECT secret FROM two_factor_credentials WHERE user_id = ?1 AND confirmed_at IS NULL",
            params![user_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(secret_base32) = secret_base32 else {
        return Err(ConfirmError::NoPendingSetup);
    };

    let totp = build_totp(&secret_base32, account_name)?;
    if totp.check_current(code).is_none() {
        return Err(ConfirmError::InvalidCode);
    }

    let tx = conn.transaction()?;
    let now = session::now();
    tx.execute(
        "UPDATE two_factor_credentials SET confirmed_at = ?1 WHERE user_id = ?2",
        params![now, user_id],
    )?;

    let mut raw_codes = Vec::with_capacity(RECOVERY_CODE_COUNT);
    for _ in 0..RECOVERY_CODE_COUNT {
        let raw = token::generate();
        tx.execute(
            "INSERT INTO two_factor_recovery_codes (id, user_id, code_hash, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                uuid::Uuid::new_v4().to_string(),
                user_id,
                token::hash(&raw),
                now
            ],
        )?;
        raw_codes.push(raw);
    }
    tx.commit()?;

    Ok(raw_codes)
}

/// `true` if this account has confirmed 2FA — the login-flow branch point
/// (`POST /auth/login`, 03-api-design.md §1).
pub fn is_enabled(conn: &Connection, user_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT count(*) > 0 FROM two_factor_credentials WHERE user_id = ?1 AND confirmed_at IS NOT NULL",
        params![user_id],
        |row| row.get(0),
    )
}

pub struct Status {
    pub enabled: bool,
    pub pending_setup: bool,
    pub recovery_codes_remaining: i64,
}

/// `GET /auth/2fa/status`.
pub fn status(conn: &Connection, user_id: &str) -> rusqlite::Result<Status> {
    let confirmed_at: Option<Option<i64>> = conn
        .query_row(
            "SELECT confirmed_at FROM two_factor_credentials WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )
        .optional()?;
    let enabled = matches!(confirmed_at, Some(Some(_)));
    let pending_setup = matches!(confirmed_at, Some(None));
    let recovery_codes_remaining = conn.query_row(
        "SELECT count(*) FROM two_factor_recovery_codes WHERE user_id = ?1 AND used_at IS NULL",
        params![user_id],
        |row| row.get(0),
    )?;
    Ok(Status {
        enabled,
        pending_setup,
        recovery_codes_remaining,
    })
}

/// Checks `code` as either a live TOTP code or an unused recovery code
/// (`POST /auth/2fa/verify`/`disable`/`recovery-codes/regenerate` all
/// accept either, 10-operations.md §2) — a matching recovery code is
/// consumed (`used_at` set) as a side effect. Returns `false` (not an
/// error) for "no confirmed credential at all" so callers can treat that
/// uniformly with "wrong code" rather than a separate branch.
fn verify_code(
    conn: &Connection,
    user_id: &str,
    account_name: &str,
    code: &str,
) -> anyhow::Result<bool> {
    let secret_base32: Option<String> = conn
        .query_row(
            "SELECT secret FROM two_factor_credentials WHERE user_id = ?1 AND confirmed_at IS NOT NULL",
            params![user_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(secret_base32) = secret_base32 {
        let totp = build_totp(&secret_base32, account_name)?;
        if totp.check_current(code).is_some() {
            return Ok(true);
        }
    }

    let code_hash = token::hash(code);
    let recovery_id: Option<String> = conn
        .query_row(
            "SELECT id FROM two_factor_recovery_codes
             WHERE user_id = ?1 AND code_hash = ?2 AND used_at IS NULL",
            params![user_id, code_hash],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(recovery_id) = recovery_id {
        conn.execute(
            "UPDATE two_factor_recovery_codes SET used_at = ?1 WHERE id = ?2",
            params![session::now(), recovery_id],
        )?;
        return Ok(true);
    }

    Ok(false)
}

#[derive(Debug, Error)]
pub enum DisableError {
    #[error("2FA is not enabled on this account")]
    NotEnabled,
    #[error("that code didn't match")]
    InvalidCode,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// `POST /auth/2fa/disable` — the API layer verifies the password
/// separately; this only handles the "and a live code" half plus the
/// actual teardown (10-operations.md §2).
pub fn disable(
    conn: &Connection,
    user_id: &str,
    account_name: &str,
    code: &str,
) -> Result<(), DisableError> {
    if !is_enabled(conn, user_id)? {
        return Err(DisableError::NotEnabled);
    }
    if !verify_code(conn, user_id, account_name, code)? {
        return Err(DisableError::InvalidCode);
    }
    force_disable_for_user(conn, user_id)?;
    Ok(())
}

/// Force-clears `two_factor_credentials` and every recovery code for
/// `user_id` — shared by `POST /auth/2fa/disable` (after its own
/// password+code check above) and `admin disable-2fa`
/// (10-operations.md §5), which needs no code check at all since the
/// CLI's trust boundary is host access, not another auth layer.
pub fn force_disable_for_user(conn: &Connection, user_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM two_factor_credentials WHERE user_id = ?1",
        params![user_id],
    )?;
    conn.execute(
        "DELETE FROM two_factor_recovery_codes WHERE user_id = ?1",
        params![user_id],
    )?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum ChallengeVerifyError {
    #[error("challenge has expired or exceeded its attempt limit — log in again")]
    ExpiredOrExhausted,
    #[error("that code didn't match")]
    InvalidCode,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

const CHALLENGE_TTL_SECS: i64 = 5 * 60;
const CHALLENGE_MAX_ATTEMPTS: i64 = 5;

/// Creates the `two_factor_login_challenges` row `POST /auth/login`
/// returns instead of a session when the account has confirmed 2FA
/// (03-api-design.md §1) — the password was necessary but not sufficient.
pub fn create_challenge(conn: &Connection, user_id: &str) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = session::now();
    conn.execute(
        "INSERT INTO two_factor_login_challenges (id, user_id, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![id, user_id, now, now + CHALLENGE_TTL_SECS],
    )?;
    Ok(id)
}

/// `POST /auth/2fa/verify` — completes a login started by a correct
/// password on a 2FA-enabled account. Returns the user id to create a
/// session for on success; deletes the challenge either way once it's
/// exhausted, so a fresh `POST /auth/login` is the only way forward
/// after that.
pub fn verify_challenge(
    conn: &Connection,
    challenge_id: &str,
    account_name_lookup: impl FnOnce(&str) -> rusqlite::Result<String>,
    code: &str,
) -> Result<String, ChallengeVerifyError> {
    let row: Option<(String, i64, i64)> = conn
        .query_row(
            "SELECT user_id, expires_at, attempts FROM two_factor_login_challenges WHERE id = ?1",
            params![challenge_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((user_id, expires_at, attempts)) = row else {
        return Err(ChallengeVerifyError::ExpiredOrExhausted);
    };

    let now = session::now();
    if expires_at < now || attempts >= CHALLENGE_MAX_ATTEMPTS {
        conn.execute(
            "DELETE FROM two_factor_login_challenges WHERE id = ?1",
            params![challenge_id],
        )?;
        return Err(ChallengeVerifyError::ExpiredOrExhausted);
    }

    let account_name = account_name_lookup(&user_id)?;
    if verify_code(conn, &user_id, &account_name, code)? {
        conn.execute(
            "DELETE FROM two_factor_login_challenges WHERE id = ?1",
            params![challenge_id],
        )?;
        Ok(user_id)
    } else {
        conn.execute(
            "UPDATE two_factor_login_challenges SET attempts = attempts + 1 WHERE id = ?1",
            params![challenge_id],
        )?;
        Err(ChallengeVerifyError::InvalidCode)
    }
}

#[derive(Debug, Error)]
pub enum RegenerateError {
    #[error("2FA is not enabled on this account")]
    NotEnabled,
    #[error("that code didn't match")]
    InvalidCode,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// `POST /auth/2fa/recovery-codes/regenerate` — invalidates every
/// existing recovery code and issues a fresh set, shown once.
pub fn regenerate_recovery_codes(
    conn: &mut Connection,
    user_id: &str,
    account_name: &str,
    code: &str,
) -> Result<Vec<String>, RegenerateError> {
    if !is_enabled(conn, user_id)? {
        return Err(RegenerateError::NotEnabled);
    }
    if !verify_code(conn, user_id, account_name, code)? {
        return Err(RegenerateError::InvalidCode);
    }

    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM two_factor_recovery_codes WHERE user_id = ?1",
        params![user_id],
    )?;
    let now = session::now();
    let mut raw_codes = Vec::with_capacity(RECOVERY_CODE_COUNT);
    for _ in 0..RECOVERY_CODE_COUNT {
        let raw = token::generate();
        tx.execute(
            "INSERT INTO two_factor_recovery_codes (id, user_id, code_hash, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                uuid::Uuid::new_v4().to_string(),
                user_id,
                token::hash(&raw),
                now
            ],
        )?;
        raw_codes.push(raw);
    }
    tx.commit()?;
    Ok(raw_codes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool() -> (tempfile::TempDir, crate::db::Pool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        (dir, pool)
    }

    fn seed_user(conn: &Connection) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH', 0)",
            params![id],
        )
        .unwrap();
        id
    }

    fn current_code(secret_base32: &str, account_name: &str) -> String {
        build_totp(secret_base32, account_name)
            .unwrap()
            .generate_current()
            .to_string()
    }

    #[test]
    fn setup_then_confirm_with_the_right_code_enables_2fa_and_issues_recovery_codes() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);

        let pending = setup(&conn, &user_id, "kh@example.test").unwrap();
        assert!(!status(&conn, &user_id).unwrap().enabled);
        assert!(status(&conn, &user_id).unwrap().pending_setup);

        let code = current_code(&pending.secret_base32, "kh@example.test");
        let codes = confirm(&mut conn, &user_id, "kh@example.test", &code).unwrap();
        assert_eq!(codes.len(), 10);

        let st = status(&conn, &user_id).unwrap();
        assert!(st.enabled);
        assert!(!st.pending_setup);
        assert_eq!(st.recovery_codes_remaining, 10);
    }

    #[test]
    fn confirm_with_the_wrong_code_does_not_enable_2fa() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);
        setup(&conn, &user_id, "kh@example.test").unwrap();

        let result = confirm(&mut conn, &user_id, "kh@example.test", "000000");
        assert!(matches!(result, Err(ConfirmError::InvalidCode)));
        assert!(!status(&conn, &user_id).unwrap().enabled);
    }

    #[test]
    fn login_challenge_round_trips_with_a_real_totp_code() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);
        let pending = setup(&conn, &user_id, "kh@example.test").unwrap();
        let code = current_code(&pending.secret_base32, "kh@example.test");
        confirm(&mut conn, &user_id, "kh@example.test", &code).unwrap();

        let challenge_id = create_challenge(&conn, &user_id).unwrap();
        let login_code = current_code(&pending.secret_base32, "kh@example.test");
        let resolved = verify_challenge(
            &conn,
            &challenge_id,
            |_| Ok("kh@example.test".to_string()),
            &login_code,
        )
        .unwrap();
        assert_eq!(resolved, user_id);

        // Challenge is one-shot.
        let second = verify_challenge(
            &conn,
            &challenge_id,
            |_| Ok("kh@example.test".to_string()),
            &login_code,
        );
        assert!(matches!(
            second,
            Err(ChallengeVerifyError::ExpiredOrExhausted)
        ));
    }

    #[test]
    fn a_recovery_code_completes_a_challenge_exactly_once() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);
        let pending = setup(&conn, &user_id, "kh@example.test").unwrap();
        let code = current_code(&pending.secret_base32, "kh@example.test");
        let recovery_codes = confirm(&mut conn, &user_id, "kh@example.test", &code).unwrap();
        let recovery_code = recovery_codes[0].clone();

        let challenge_id = create_challenge(&conn, &user_id).unwrap();
        let resolved = verify_challenge(
            &conn,
            &challenge_id,
            |_| Ok("kh@example.test".to_string()),
            &recovery_code,
        )
        .unwrap();
        assert_eq!(resolved, user_id);

        let challenge_id_2 = create_challenge(&conn, &user_id).unwrap();
        let second = verify_challenge(
            &conn,
            &challenge_id_2,
            |_| Ok("kh@example.test".to_string()),
            &recovery_code,
        );
        assert!(matches!(second, Err(ChallengeVerifyError::InvalidCode)));
    }

    #[test]
    fn force_disable_clears_credential_and_recovery_codes() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);
        let pending = setup(&conn, &user_id, "kh@example.test").unwrap();
        let code = current_code(&pending.secret_base32, "kh@example.test");
        confirm(&mut conn, &user_id, "kh@example.test", &code).unwrap();

        force_disable_for_user(&conn, &user_id).unwrap();
        assert!(!status(&conn, &user_id).unwrap().enabled);
        assert_eq!(status(&conn, &user_id).unwrap().recovery_codes_remaining, 0);
    }

    #[test]
    fn disable_requires_a_valid_code() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);
        let pending = setup(&conn, &user_id, "kh@example.test").unwrap();
        let code = current_code(&pending.secret_base32, "kh@example.test");
        confirm(&mut conn, &user_id, "kh@example.test", &code).unwrap();

        let result = disable(&conn, &user_id, "kh@example.test", "000000");
        assert!(matches!(result, Err(DisableError::InvalidCode)));
        assert!(status(&conn, &user_id).unwrap().enabled);
    }

    #[test]
    fn regenerating_recovery_codes_invalidates_the_old_set() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);
        let pending = setup(&conn, &user_id, "kh@example.test").unwrap();
        let code = current_code(&pending.secret_base32, "kh@example.test");
        let old_codes = confirm(&mut conn, &user_id, "kh@example.test", &code).unwrap();

        let fresh_code = current_code(&pending.secret_base32, "kh@example.test");
        let new_codes =
            regenerate_recovery_codes(&mut conn, &user_id, "kh@example.test", &fresh_code).unwrap();
        assert_eq!(new_codes.len(), 10);
        assert_ne!(old_codes[0], new_codes[0]);

        let challenge_id = create_challenge(&conn, &user_id).unwrap();
        let result = verify_challenge(
            &conn,
            &challenge_id,
            |_| Ok("kh@example.test".to_string()),
            &old_codes[0],
        );
        assert!(matches!(result, Err(ChallengeVerifyError::InvalidCode)));
    }
}
