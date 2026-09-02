//! Invite-only submissive signup (01-data-model.md §2): a Keyholder
//! issues a token, a submissive redeems it into an account plus an
//! active link — the only way a submissive account ever comes to exist.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::auth::token;
use crate::domain::{audit, links, users};

const DEFAULT_EXPIRES_IN_HOURS: i64 = 48;

pub struct CreatedInvite {
    // Not read by the API response (03-api-design.md §2 only returns
    // token/expires_at) — kept for callers (and tests) that need to
    // target this exact row without re-deriving it from the token.
    #[allow(dead_code)]
    pub id: String,
    pub token: String,
    pub expires_at: i64,
}

pub fn create(
    conn: &Connection,
    keyholder_id: &str,
    expires_in_hours: Option<i64>,
) -> rusqlite::Result<CreatedInvite> {
    let id = uuid::Uuid::new_v4().to_string();
    let raw_token = token::generate();
    let now = crate::auth::session::now();
    let expires_at = now + expires_in_hours.unwrap_or(DEFAULT_EXPIRES_IN_HOURS) * 3600;

    conn.execute(
        "INSERT INTO invites (id, token_hash, created_by_keyholder_id, expires_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![id, token::hash(&raw_token), keyholder_id, expires_at],
    )?;

    Ok(CreatedInvite {
        id,
        token: raw_token,
        expires_at,
    })
}

pub struct InviteSummary {
    pub id: String,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub used_by_user_id: Option<String>,
}

pub fn list_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<InviteSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, expires_at, used_at, used_by_user_id
         FROM invites WHERE created_by_keyholder_id = ?1
         ORDER BY expires_at DESC",
    )?;
    stmt.query_map(params![keyholder_id], |row| {
        Ok(InviteSummary {
            id: row.get(0)?,
            expires_at: row.get(1)?,
            used_at: row.get(2)?,
            used_by_user_id: row.get(3)?,
        })
    })?
    .collect()
}

/// Deletes an unused invite belonging to `keyholder_id` — there's no
/// `revoked_at` column (invites.used_at/used_by_user_id are the only
/// state), so revoking one that was never redeemed is a plain delete.
/// Returns `false` if no matching unused invite was found (already used,
/// already gone, or not this Keyholder's).
pub fn revoke(conn: &Connection, invite_id: &str, keyholder_id: &str) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "DELETE FROM invites
         WHERE id = ?1 AND created_by_keyholder_id = ?2 AND used_at IS NULL",
        params![invite_id, keyholder_id],
    )?;
    Ok(affected > 0)
}

#[derive(Debug, Error)]
pub enum RedeemError {
    #[error("invite token is invalid, expired, or already used")]
    InvalidOrExpired,
    #[error("email already in use")]
    EmailInUse,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub struct RedeemedAccount {
    pub user_id: String,
    pub link_id: String,
}

pub struct Redeem<'a> {
    pub token: &'a str,
    pub email: &'a str,
    pub password_hash: &'a str,
    pub display_name: &'a str,
}

/// Validates the token, creates the submissive account, creates the
/// `active` link (and its default verification policy via
/// `links::create`), and marks the invite used — all in one transaction
/// (03-api-design.md §1).
pub fn redeem(conn: &mut Connection, redeem: Redeem) -> Result<RedeemedAccount, RedeemError> {
    let now = crate::auth::session::now();
    let tx = conn.transaction()?;

    let invite_row: Option<(String, String)> = tx
        .query_row(
            "SELECT id, created_by_keyholder_id FROM invites
             WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
            params![token::hash(redeem.token), now],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((invite_id, keyholder_id)) = invite_row else {
        return Err(RedeemError::InvalidOrExpired);
    };

    let user_id = match users::create_submissive(
        &tx,
        users::NewAccount {
            email: redeem.email,
            password_hash: redeem.password_hash,
            display_name: redeem.display_name,
        },
    ) {
        Ok(id) => id,
        Err(users::CreateUserError::EmailInUse) => return Err(RedeemError::EmailInUse),
        Err(users::CreateUserError::Db(e)) => return Err(e.into()),
    };

    let link_id = links::create(&tx, &keyholder_id, &user_id)?;

    tx.execute(
        "UPDATE invites SET used_at = ?1, used_by_user_id = ?2 WHERE id = ?3",
        params![now, user_id, invite_id],
    )?;

    audit::record(
        &tx,
        audit::Entry {
            actor: audit::Actor::User(&user_id),
            link_id: Some(&link_id),
            action: "invite.redeemed",
            entity_type: "invites",
            entity_id: &invite_id,
            detail: None,
        },
    )?;

    tx.commit()?;
    Ok(RedeemedAccount { user_id, link_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_keyholder(conn: &Connection) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH', 0)",
            params![id],
        )
        .unwrap();
        id
    }

    #[test]
    fn create_then_redeem_creates_account_and_active_link() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let mut conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);

        let invite = create(&conn, &keyholder_id, None).unwrap();

        let redeemed = redeem(
            &mut conn,
            Redeem {
                token: &invite.token,
                email: "new-sub@example.test",
                password_hash: "hash",
                display_name: "New Sub",
            },
        )
        .unwrap();

        let link_status: String = conn
            .query_row(
                "SELECT status FROM keyholder_submissive_links WHERE id = ?1",
                params![redeemed.link_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(link_status, "active");
    }

    #[test]
    fn redeeming_twice_fails_the_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let mut conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);
        let invite = create(&conn, &keyholder_id, None).unwrap();

        redeem(
            &mut conn,
            Redeem {
                token: &invite.token,
                email: "first@example.test",
                password_hash: "hash",
                display_name: "First",
            },
        )
        .unwrap();

        let second = redeem(
            &mut conn,
            Redeem {
                token: &invite.token,
                email: "second@example.test",
                password_hash: "hash",
                display_name: "Second",
            },
        );
        assert!(matches!(second, Err(RedeemError::InvalidOrExpired)));
    }

    #[test]
    fn expired_invite_cannot_be_redeemed() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let mut conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);
        let invite = create(&conn, &keyholder_id, Some(1)).unwrap();
        conn.execute(
            "UPDATE invites SET expires_at = 0 WHERE id = ?1",
            params![invite.id],
        )
        .unwrap();

        let result = redeem(
            &mut conn,
            Redeem {
                token: &invite.token,
                email: "toolate@example.test",
                password_hash: "hash",
                display_name: "Too Late",
            },
        );
        assert!(matches!(result, Err(RedeemError::InvalidOrExpired)));
    }

    #[test]
    fn revoke_only_deletes_the_owning_keyholders_unused_invite() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);
        let other_keyholder_id = seed_keyholder(&conn);
        let invite = create(&conn, &keyholder_id, None).unwrap();

        assert!(!revoke(&conn, &invite.id, &other_keyholder_id).unwrap());
        assert!(revoke(&conn, &invite.id, &keyholder_id).unwrap());
        assert!(list_for_keyholder(&conn, &keyholder_id).unwrap().is_empty());
    }

    #[test]
    fn list_for_keyholder_only_returns_own_invites() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);
        let other_keyholder_id = seed_keyholder(&conn);
        create(&conn, &keyholder_id, None).unwrap();
        create(&conn, &other_keyholder_id, None).unwrap();

        let mine = list_for_keyholder(&conn, &keyholder_id).unwrap();
        assert_eq!(mine.len(), 1);
    }
}
