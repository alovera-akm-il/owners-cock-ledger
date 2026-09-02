//! Account recovery via a single-use token (01-data-model.md §2,
//! 10-operations.md §5). Two issuance paths — `admin reset-password`
//! (`RequestedVia::AdminCli`) and, if the deployer opts into outbound
//! email, self-service (`RequestedVia::SelfService`) — write the same
//! table and are redeemed through the identical path below; the redeem
//! step has no way to know or care which one created the row it consumes.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::auth::{session, token};

const EXPIRES_IN_HOURS: i64 = 24;

pub enum RequestedVia {
    AdminCli,
    // No caller yet: `POST /auth/password-reset/request` is a permanent
    // no-op until this deployment wires up outbound SMTP (see that
    // endpoint's doc comment in `api::auth`) — this variant documents
    // the schema's other issuance path ahead of that integration
    // existing, rather than the table only ever seeing `admin_cli`.
    #[allow(dead_code)]
    SelfService,
}

impl RequestedVia {
    fn as_str(&self) -> &'static str {
        match self {
            RequestedVia::AdminCli => "admin_cli",
            RequestedVia::SelfService => "self_service",
        }
    }
}

pub struct IssuedToken {
    pub token: String,
    // Not read by any caller yet (neither the admin CLI nor a future
    // emailed link surfaces an explicit expiry) — kept on the struct
    // since it's already computed and a natural thing to want later.
    #[allow(dead_code)]
    pub expires_at: i64,
}

/// Issues a fresh single-use reset token for `user_id`. Doesn't check
/// whether the account exists — callers that need enumeration-safety
/// (the self-service request endpoint) do that check themselves and
/// simply don't call this when it doesn't.
pub fn issue(conn: &Connection, user_id: &str, via: RequestedVia) -> rusqlite::Result<IssuedToken> {
    let raw_token = token::generate();
    let now = session::now();
    let expires_at = now + EXPIRES_IN_HOURS * 3600;

    conn.execute(
        "INSERT INTO password_reset_tokens (id, user_id, token_hash, requested_via, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            uuid::Uuid::new_v4().to_string(),
            user_id,
            token::hash(&raw_token),
            via.as_str(),
            now,
            expires_at,
        ],
    )?;

    Ok(IssuedToken {
        token: raw_token,
        expires_at,
    })
}

#[derive(Debug, Error)]
pub enum RedeemError {
    #[error("reset token is invalid, expired, or already used")]
    InvalidOrExpired,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// Validates the token, sets the new password hash, consumes the token,
/// and revokes every existing session for the account — all in one
/// transaction (`POST /auth/password-reset/redeem`, 10-operations.md §5).
/// Returns the affected user's id.
pub fn redeem(
    conn: &mut Connection,
    raw_token: &str,
    new_password_hash: &str,
) -> Result<String, RedeemError> {
    let now = session::now();
    let tx = conn.transaction()?;

    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT id, user_id FROM password_reset_tokens
             WHERE token_hash = ?1 AND consumed_at IS NULL AND expires_at > ?2",
            params![token::hash(raw_token), now],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((token_id, user_id)) = row else {
        return Err(RedeemError::InvalidOrExpired);
    };

    tx.execute(
        "UPDATE users SET password_hash = ?1 WHERE id = ?2",
        params![new_password_hash, user_id],
    )?;
    tx.execute(
        "UPDATE password_reset_tokens SET consumed_at = ?1 WHERE id = ?2",
        params![now, token_id],
    )?;
    session::revoke_all(&tx, &user_id)?;

    tx.commit()?;
    Ok(user_id)
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
             VALUES (?1, ?1 || '@example.test', 'old-hash', 'keyholder', 'KH', 0)",
            params![id],
        )
        .unwrap();
        id
    }

    #[test]
    fn issue_then_redeem_sets_the_new_password_hash() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);

        let issued = issue(&conn, &user_id, RequestedVia::AdminCli).unwrap();
        let redeemed_user_id = redeem(&mut conn, &issued.token, "new-hash").unwrap();
        assert_eq!(redeemed_user_id, user_id);

        let hash: String = conn
            .query_row(
                "SELECT password_hash FROM users WHERE id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hash, "new-hash");
    }

    #[test]
    fn redeeming_twice_fails_the_second_time() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);
        let issued = issue(&conn, &user_id, RequestedVia::SelfService).unwrap();

        redeem(&mut conn, &issued.token, "new-hash").unwrap();
        let second = redeem(&mut conn, &issued.token, "another-hash");
        assert!(matches!(second, Err(RedeemError::InvalidOrExpired)));
    }

    #[test]
    fn expired_token_cannot_be_redeemed() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);
        let issued = issue(&conn, &user_id, RequestedVia::AdminCli).unwrap();
        conn.execute(
            "UPDATE password_reset_tokens SET expires_at = 0 WHERE user_id = ?1",
            params![user_id],
        )
        .unwrap();

        let result = redeem(&mut conn, &issued.token, "new-hash");
        assert!(matches!(result, Err(RedeemError::InvalidOrExpired)));
    }

    #[test]
    fn redeeming_revokes_every_existing_session() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let user_id = seed_user(&conn);
        let session_id = session::create(&conn, &user_id, None).unwrap();
        let issued = issue(&conn, &user_id, RequestedVia::AdminCli).unwrap();

        redeem(&mut conn, &issued.token, "new-hash").unwrap();

        assert!(session::resolve(&conn, &session_id).unwrap().is_none());
    }
}
