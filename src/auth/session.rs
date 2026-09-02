//! Server-side session-cookie auth (05-security-and-privacy.md §2,
//! `sessions` table in 01-data-model.md §2). The cookie carries only an
//! opaque, unguessable session id — revocation is a row delete, not a
//! signature check, which is the whole point of choosing this over a JWT.
//!
//! Phase 0 wires this in as infrastructure ahead of Phase 1's actual login
//! endpoint (`15-implementation-roadmap.md` §4), so most of it has no
//! caller yet outside its own tests.
#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{FromRef, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use rusqlite::{Connection, OptionalExtension, params};

use crate::db::Pool;

pub const SESSION_COOKIE_NAME: &str = "ocl_session";
const SESSION_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60; // 30 days
const LAST_SEEN_THROTTLE_SECS: i64 = 60;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Keyholder,
    Submissive,
}

impl Role {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "keyholder" => Some(Role::Keyholder),
            "submissive" => Some(Role::Submissive),
            _ => None,
        }
    }
}

/// The authenticated caller, resolved from the session cookie
/// (02-roles-and-permissions.md §1 principle 4: middleware resolves
/// `(user, role)` on every request; the handler decides which role(s) may
/// proceed).
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub user_id: String,
    pub role: Role,
    pub display_name: String,
    pub session_id: String,
}

impl CurrentUser {
    /// Rejects with `403` when the caller's role isn't one of the allowed
    /// roles for this handler — the explicit per-handler declaration
    /// principle 4 calls for, rather than baking role checks into the
    /// extractor itself.
    pub fn require_role(&self, allowed: &[Role]) -> Result<(), StatusCode> {
        if allowed.contains(&self.role) {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// Creates a new session row for `user_id` and returns the opaque id to
/// set as the cookie value.
pub fn create(
    conn: &Connection,
    user_id: &str,
    user_agent: Option<&str>,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let created_at = now();
    conn.execute(
        "INSERT INTO sessions (id, user_id, created_at, last_seen_at, expires_at, user_agent)
         VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
        params![
            id,
            user_id,
            created_at,
            created_at + SESSION_LIFETIME_SECS,
            user_agent,
        ],
    )?;
    Ok(id)
}

/// Resolves a session id to the authenticated user, or `None` if the
/// session doesn't exist, is revoked, or has expired. Touches
/// `last_seen_at`, throttled to at most once a minute per session
/// (01-data-model.md §2) so an active session doesn't write on every
/// single request.
pub fn resolve(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<CurrentUser>> {
    let row = conn
        .query_row(
            "SELECT s.user_id, s.expires_at, s.revoked_at, s.last_seen_at,
                    u.role, u.display_name, u.disabled_at
             FROM sessions s
             JOIN users u ON u.id = s.user_id
             WHERE s.id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            },
        )
        .optional()?;

    let Some((user_id, expires_at, revoked_at, last_seen_at, role, display_name, disabled_at)) =
        row
    else {
        return Ok(None);
    };

    let current = now();
    if revoked_at.is_some() || expires_at < current || disabled_at.is_some() {
        return Ok(None);
    }

    if current - last_seen_at >= LAST_SEEN_THROTTLE_SECS {
        conn.execute(
            "UPDATE sessions SET last_seen_at = ?1 WHERE id = ?2",
            params![current, session_id],
        )?;
    }

    let Some(role) = Role::parse(&role) else {
        return Ok(None);
    };

    Ok(Some(CurrentUser {
        user_id,
        role,
        display_name,
        session_id: session_id.to_string(),
    }))
}

/// Revokes one session (explicit logout, `DELETE /auth/sessions/{id}`).
pub fn revoke(conn: &Connection, session_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET revoked_at = ?1 WHERE id = ?2",
        params![now(), session_id],
    )?;
    Ok(())
}

/// Revokes every *other* session for a user — used after a password
/// change/reset (10-operations.md §1) and never called with the caller's
/// own current session id.
pub fn revoke_all_except(
    conn: &Connection,
    user_id: &str,
    except_session_id: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE sessions SET revoked_at = ?1
         WHERE user_id = ?2 AND id != ?3 AND revoked_at IS NULL",
        params![now(), user_id, except_session_id],
    )
}

/// One row of `GET /auth/sessions` (10-operations.md §1) — `is_current`
/// isn't stored, it's computed by the caller comparing `id` against their
/// own `CurrentUser.session_id`.
pub struct SessionSummary {
    pub id: String,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub user_agent: Option<String>,
}

/// A user's own active (unrevoked, unexpired) sessions, most recently
/// active first.
pub fn list_for_user(conn: &Connection, user_id: &str) -> rusqlite::Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, created_at, last_seen_at, user_agent FROM sessions
         WHERE user_id = ?1 AND revoked_at IS NULL AND expires_at > ?2
         ORDER BY last_seen_at DESC",
    )?;
    stmt.query_map(params![user_id, now()], |row| {
        Ok(SessionSummary {
            id: row.get(0)?,
            created_at: row.get(1)?,
            last_seen_at: row.get(2)?,
            user_agent: row.get(3)?,
        })
    })?
    .collect()
}

/// Revokes one session, scoped to `user_id` so a caller can never revoke
/// someone else's session by guessing an id (`DELETE /auth/sessions/{id}`,
/// 03-api-design.md §1). Returns `false` if no matching, still-active
/// session was found.
pub fn revoke_own(conn: &Connection, session_id: &str, user_id: &str) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "UPDATE sessions SET revoked_at = ?1
         WHERE id = ?2 AND user_id = ?3 AND revoked_at IS NULL",
        params![now(), session_id, user_id],
    )?;
    Ok(affected > 0)
}

impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
    Pool: axum::extract::FromRef<S>,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let session_id = jar
            .get(SESSION_COOKIE_NAME)
            .map(|c| c.value().to_string())
            .ok_or(StatusCode::UNAUTHORIZED)?;

        let pool = Pool::from_ref(state);
        let user = tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            resolve(&conn, &session_id).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;

        user.ok_or(StatusCode::UNAUTHORIZED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_user(conn: &Connection, role: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?2, 'hash', ?3, 'Test User', ?4)",
            params![id, format!("{id}@example.test"), role, now()],
        )
        .unwrap();
        id
    }

    #[test]
    fn create_then_resolve_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let user_id = seed_user(&conn, "keyholder");

        let session_id = create(&conn, &user_id, Some("test-agent")).unwrap();
        let resolved = resolve(&conn, &session_id).unwrap().unwrap();

        assert_eq!(resolved.user_id, user_id);
        assert_eq!(resolved.role, Role::Keyholder);
    }

    #[test]
    fn revoked_session_does_not_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let user_id = seed_user(&conn, "submissive");

        let session_id = create(&conn, &user_id, None).unwrap();
        revoke(&conn, &session_id).unwrap();

        assert!(resolve(&conn, &session_id).unwrap().is_none());
    }

    #[test]
    fn expired_session_does_not_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let user_id = seed_user(&conn, "submissive");

        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO sessions (id, user_id, created_at, last_seen_at, expires_at)
             VALUES (?1, ?2, ?3, ?3, ?3 - 1)",
            params![id, user_id, now()],
        )
        .unwrap();

        assert!(resolve(&conn, &id).unwrap().is_none());
    }

    #[test]
    fn nonexistent_session_does_not_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        assert!(resolve(&conn, "not-a-real-session-id").unwrap().is_none());
    }

    #[test]
    fn revoke_all_except_leaves_the_named_session_alone() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let user_id = seed_user(&conn, "keyholder");

        let keep = create(&conn, &user_id, None).unwrap();
        let drop_a = create(&conn, &user_id, None).unwrap();
        let drop_b = create(&conn, &user_id, None).unwrap();

        let revoked = revoke_all_except(&conn, &user_id, &keep).unwrap();
        assert_eq!(revoked, 2);

        assert!(resolve(&conn, &keep).unwrap().is_some());
        assert!(resolve(&conn, &drop_a).unwrap().is_none());
        assert!(resolve(&conn, &drop_b).unwrap().is_none());
    }

    #[test]
    fn require_role_rejects_the_wrong_role() {
        let user = CurrentUser {
            user_id: "u1".into(),
            role: Role::Submissive,
            display_name: "Test".into(),
            session_id: "s1".into(),
        };
        assert!(user.require_role(&[Role::Keyholder]).is_err());
        assert!(user.require_role(&[Role::Submissive]).is_ok());
    }
}
