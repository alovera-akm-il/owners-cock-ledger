//! Core identity (01-data-model.md §2): account creation and the login
//! lockout mechanism. Profile-field editing (03-api-design.md §3) isn't
//! built yet — Phase 1 only needs enough of `users`/`*_profiles` to
//! create accounts and log in.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

/// Consecutive failed attempts before an account locks. Not specified
/// numerically anywhere in the docs (05-security-and-privacy.md §2 only
/// says "locks after threshold") — picked as a reasonable default rather
/// than left undecided.
const LOCKOUT_THRESHOLD: i64 = 10;
const LOCKOUT_DURATION_SECS: i64 = 15 * 60;

#[derive(Debug, Error)]
pub enum CreateUserError {
    #[error("email already in use")]
    EmailInUse,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub struct NewAccount<'a> {
    pub email: &'a str,
    pub password_hash: &'a str,
    pub display_name: &'a str,
}

pub fn create_keyholder(conn: &Connection, new: NewAccount) -> Result<String, CreateUserError> {
    create(conn, new, "keyholder", "keyholder_profiles")
}

pub fn create_submissive(conn: &Connection, new: NewAccount) -> Result<String, CreateUserError> {
    create(conn, new, "submissive", "submissive_profiles")
}

fn create(
    conn: &Connection,
    new: NewAccount,
    role: &str,
    profile_table: &str,
) -> Result<String, CreateUserError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::auth::session::now();
    let result = conn.execute(
        "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            new.email,
            new.password_hash,
            role,
            new.display_name,
            now
        ],
    );
    match result {
        Ok(_) => {
            conn.execute(
                &format!("INSERT INTO {profile_table} (user_id) VALUES (?1)"),
                params![id],
            )?;
            Ok(id)
        }
        Err(e) if super::is_unique_violation(&e) => Err(CreateUserError::EmailInUse),
        Err(e) => Err(e.into()),
    }
}

pub struct AccountForLogin {
    pub id: String,
    pub password_hash: String,
    pub role: String,
    pub display_name: String,
    // Mirrors the users row in full even though login only ever consults
    // locked_until (via is_locked) — kept available for e.g. an "N
    // attempts remaining" message later without another query.
    #[allow(dead_code)]
    pub failed_login_count: i64,
    pub locked_until: Option<i64>,
    pub disabled_at: Option<i64>,
}

pub fn find_by_email(conn: &Connection, email: &str) -> rusqlite::Result<Option<AccountForLogin>> {
    conn.query_row(
        "SELECT id, password_hash, role, display_name, failed_login_count, locked_until, disabled_at
         FROM users WHERE email = ?1",
        params![email],
        |row| {
            Ok(AccountForLogin {
                id: row.get(0)?,
                password_hash: row.get(1)?,
                role: row.get(2)?,
                display_name: row.get(3)?,
                failed_login_count: row.get(4)?,
                locked_until: row.get(5)?,
                disabled_at: row.get(6)?,
            })
        },
    )
    .optional()
}

/// `true` if the account is currently locked out (05-security-and-privacy.md §2).
pub fn is_locked(account: &AccountForLogin, now: i64) -> bool {
    account.locked_until.is_some_and(|until| until > now)
}

/// Increments the failure counter and locks the account once it crosses
/// `LOCKOUT_THRESHOLD`.
pub fn record_failed_login(conn: &Connection, user_id: &str) -> rusqlite::Result<()> {
    let now = crate::auth::session::now();
    conn.execute(
        "UPDATE users SET
            failed_login_count = failed_login_count + 1,
            locked_until = CASE
                WHEN failed_login_count + 1 >= ?1 THEN ?2
                ELSE locked_until
            END
         WHERE id = ?3",
        params![LOCKOUT_THRESHOLD, now + LOCKOUT_DURATION_SECS, user_id],
    )?;
    Ok(())
}

/// Clears the failure counter on a successful login.
pub fn record_successful_login(conn: &Connection, user_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET failed_login_count = 0, locked_until = NULL WHERE id = ?1",
        params![user_id],
    )?;
    Ok(())
}

/// `admin unlock-account <email>` (10-operations.md §5) — a convenience,
/// not a necessity: the account already self-unlocks once `locked_until`
/// passes. Same effect as a successful login clearing the counter, under
/// a name that reflects why the CLI is calling it.
pub fn unlock(conn: &Connection, user_id: &str) -> rusqlite::Result<()> {
    record_successful_login(conn, user_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool() -> (tempfile::TempDir, crate::db::Pool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        (dir, pool)
    }

    #[test]
    fn create_keyholder_also_creates_an_empty_profile_row() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let id = create_keyholder(
            &conn,
            NewAccount {
                email: "kh@example.test",
                password_hash: "hash",
                display_name: "KH",
            },
        )
        .unwrap();

        let profile_exists: bool = conn
            .query_row(
                "SELECT count(*) > 0 FROM keyholder_profiles WHERE user_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(profile_exists);
    }

    #[test]
    fn duplicate_email_is_rejected_cleanly() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        create_keyholder(
            &conn,
            NewAccount {
                email: "dup@example.test",
                password_hash: "hash",
                display_name: "First",
            },
        )
        .unwrap();

        let result = create_keyholder(
            &conn,
            NewAccount {
                email: "dup@example.test",
                password_hash: "hash",
                display_name: "Second",
            },
        );
        assert!(matches!(result, Err(CreateUserError::EmailInUse)));
    }

    #[test]
    fn find_by_email_returns_none_for_unknown_address() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        assert!(
            find_by_email(&conn, "nobody@example.test")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn account_locks_after_threshold_failed_logins() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let id = create_keyholder(
            &conn,
            NewAccount {
                email: "lockout@example.test",
                password_hash: "hash",
                display_name: "KH",
            },
        )
        .unwrap();

        for _ in 0..LOCKOUT_THRESHOLD {
            record_failed_login(&conn, &id).unwrap();
        }

        let account = find_by_email(&conn, "lockout@example.test")
            .unwrap()
            .unwrap();
        assert!(is_locked(&account, crate::auth::session::now()));
    }

    #[test]
    fn successful_login_clears_the_counter() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let id = create_keyholder(
            &conn,
            NewAccount {
                email: "clears@example.test",
                password_hash: "hash",
                display_name: "KH",
            },
        )
        .unwrap();

        record_failed_login(&conn, &id).unwrap();
        record_successful_login(&conn, &id).unwrap();

        let account = find_by_email(&conn, "clears@example.test")
            .unwrap()
            .unwrap();
        assert_eq!(account.failed_login_count, 0);
        assert!(account.locked_until.is_none());
    }
}
