//! Keyholder-issued API tokens for automation (01-data-model.md §9,
//! 03-api-design.md §12) — opaque and server-revocable like a session,
//! not a stateless JWT, and Keyholder-only in v1: there's no
//! submissive-issued equivalent.

use rusqlite::{Connection, OptionalExtension, params};

use crate::auth::{session, token};

/// The `token_prefix` shown alongside a token in listings, so a
/// Keyholder can tell tokens apart without ever seeing the full value
/// again — long enough to be recognizable, short enough to leak nothing
/// useful about the rest of the token.
const PREFIX_LEN: usize = 8;

fn encode_scopes(scopes: &[String]) -> String {
    serde_json::to_string(scopes).expect("Vec<String> always serializes")
}

fn decode_scopes(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

pub struct CreatedToken {
    pub id: String,
    pub token: String,
    pub prefix: String,
    pub expires_at: Option<i64>,
}

/// `POST /keyholder/api-tokens` — the full raw token is returned exactly
/// once, in this response only (05-security-and-privacy.md §2); only its
/// hash is ever stored.
pub fn create(
    conn: &Connection,
    keyholder_id: &str,
    label: &str,
    scopes: &[String],
    expires_in_days: Option<i64>,
) -> rusqlite::Result<CreatedToken> {
    let id = uuid::Uuid::new_v4().to_string();
    let raw_token = token::generate();
    let prefix = raw_token[..PREFIX_LEN].to_string();
    let now = session::now();
    let expires_at = expires_in_days.map(|days| now + days * 86_400);

    conn.execute(
        "INSERT INTO api_tokens (id, keyholder_id, label, token_prefix, token_hash, scopes, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            keyholder_id,
            label,
            prefix,
            token::hash(&raw_token),
            encode_scopes(scopes),
            now,
            expires_at,
        ],
    )?;

    Ok(CreatedToken {
        id,
        token: raw_token,
        prefix,
        expires_at,
    })
}

pub struct TokenSummary {
    pub id: String,
    pub label: String,
    pub token_prefix: String,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

/// `GET /keyholder/api-tokens` — never returns the full token value.
pub fn list_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<TokenSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, label, token_prefix, scopes, created_at, expires_at, last_used_at, revoked_at
         FROM api_tokens WHERE keyholder_id = ?1 ORDER BY created_at DESC",
    )?;
    stmt.query_map(params![keyholder_id], |row| {
        Ok(TokenSummary {
            id: row.get(0)?,
            label: row.get(1)?,
            token_prefix: row.get(2)?,
            scopes: decode_scopes(&row.get::<_, String>(3)?),
            created_at: row.get(4)?,
            expires_at: row.get(5)?,
            last_used_at: row.get(6)?,
            revoked_at: row.get(7)?,
        })
    })?
    .collect()
}

/// `PATCH /keyholder/api-tokens/{id}` — narrowing or renaming without
/// rotating; widening scopes is allowed too, since it's the same
/// Keyholder granting themself the access (03-api-design.md §12).
/// Returns `false` if no such token belongs to this Keyholder.
pub fn update(
    conn: &Connection,
    token_id: &str,
    keyholder_id: &str,
    label: Option<&str>,
    scopes: Option<&[String]>,
) -> rusqlite::Result<bool> {
    let scopes_json = scopes.map(encode_scopes);
    let affected = conn.execute(
        "UPDATE api_tokens SET
            label = COALESCE(?1, label),
            scopes = COALESCE(?2, scopes)
         WHERE id = ?3 AND keyholder_id = ?4",
        params![label, scopes_json, token_id, keyholder_id],
    )?;
    Ok(affected > 0)
}

/// `DELETE /keyholder/api-tokens/{id}` — sets `revoked_at` rather than
/// hard-deleting (history stays visible, 01-data-model.md §9). Immediate:
/// a revoked token fails auth on its very next request.
pub fn revoke(conn: &Connection, token_id: &str, keyholder_id: &str) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "UPDATE api_tokens SET revoked_at = ?1
         WHERE id = ?2 AND keyholder_id = ?3 AND revoked_at IS NULL",
        params![session::now(), token_id, keyholder_id],
    )?;
    Ok(affected > 0)
}

pub struct ResolvedToken {
    pub keyholder_id: String,
    pub display_name: String,
    pub scopes: Vec<String>,
}

/// Resolves a raw bearer token to its owning Keyholder and scopes, the
/// same shape a session resolves to `(user_id, role)`
/// (03-api-design.md §12) — `None` for missing, revoked, or expired,
/// same "caller can't tell which" posture as session resolution.
/// Updates `last_used_at` best-effort on a hit, not as part of any
/// caller's own transaction.
type TokenRow = (String, String, String, Option<i64>, Option<i64>);

pub fn resolve(conn: &Connection, raw_token: &str) -> rusqlite::Result<Option<ResolvedToken>> {
    let hash = token::hash(raw_token);
    let row: Option<TokenRow> = conn
        .query_row(
            "SELECT id, keyholder_id, scopes, expires_at, revoked_at
             FROM api_tokens WHERE token_hash = ?1",
            params![hash],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((token_id, keyholder_id, scopes_json, expires_at, revoked_at)) = row else {
        return Ok(None);
    };

    let now = session::now();
    if revoked_at.is_some() || expires_at.is_some_and(|expires_at| expires_at < now) {
        return Ok(None);
    }

    let display_name: String = conn.query_row(
        "SELECT display_name FROM users WHERE id = ?1",
        params![keyholder_id],
        |row| row.get(0),
    )?;

    let _ = conn.execute(
        "UPDATE api_tokens SET last_used_at = ?1 WHERE id = ?2",
        params![now, token_id],
    );

    Ok(Some(ResolvedToken {
        keyholder_id,
        display_name,
        scopes: decode_scopes(&scopes_json),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool() -> (tempfile::TempDir, crate::db::Pool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        (dir, pool)
    }

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
    fn create_then_resolve_round_trips_keyholder_and_scopes() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);

        let created = create(
            &conn,
            &keyholder_id,
            "notifier bot",
            &["read:submissives".to_string()],
            None,
        )
        .unwrap();

        let resolved = resolve(&conn, &created.token).unwrap().unwrap();
        assert_eq!(resolved.keyholder_id, keyholder_id);
        assert_eq!(resolved.scopes, vec!["read:submissives".to_string()]);
        assert_eq!(resolved.display_name, "KH");
    }

    #[test]
    fn resolve_updates_last_used_at() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);
        let created = create(&conn, &keyholder_id, "bot", &[], None).unwrap();

        resolve(&conn, &created.token).unwrap();

        let summary = &list_for_keyholder(&conn, &keyholder_id).unwrap()[0];
        assert!(summary.last_used_at.is_some());
    }

    #[test]
    fn a_revoked_token_no_longer_resolves() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);
        let created = create(&conn, &keyholder_id, "bot", &[], None).unwrap();
        let token_id = list_for_keyholder(&conn, &keyholder_id).unwrap()[0]
            .id
            .clone();

        assert!(revoke(&conn, &token_id, &keyholder_id).unwrap());
        assert!(resolve(&conn, &created.token).unwrap().is_none());
    }

    #[test]
    fn an_expired_token_no_longer_resolves() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);
        let created = create(&conn, &keyholder_id, "bot", &[], Some(1)).unwrap();
        conn.execute(
            "UPDATE api_tokens SET expires_at = 0 WHERE keyholder_id = ?1",
            params![keyholder_id],
        )
        .unwrap();

        assert!(resolve(&conn, &created.token).unwrap().is_none());
    }

    #[test]
    fn update_can_narrow_scopes_and_is_scoped_to_the_owning_keyholder() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);
        let other_keyholder_id = seed_keyholder(&conn);
        create(
            &conn,
            &keyholder_id,
            "bot",
            &["read:submissives".to_string(), "manage:invites".to_string()],
            None,
        )
        .unwrap();
        let token_id = list_for_keyholder(&conn, &keyholder_id).unwrap()[0]
            .id
            .clone();

        assert!(!update(&conn, &token_id, &other_keyholder_id, None, Some(&[]),).unwrap());

        assert!(
            update(
                &conn,
                &token_id,
                &keyholder_id,
                Some("renamed bot"),
                Some(&["read:submissives".to_string()]),
            )
            .unwrap()
        );

        let summary = &list_for_keyholder(&conn, &keyholder_id).unwrap()[0];
        assert_eq!(summary.label, "renamed bot");
        assert_eq!(summary.scopes, vec!["read:submissives".to_string()]);
    }

    #[test]
    fn a_token_with_no_scopes_resolves_but_grants_nothing() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let keyholder_id = seed_keyholder(&conn);
        let created = create(&conn, &keyholder_id, "bare bot", &[], None).unwrap();

        let resolved = resolve(&conn, &created.token).unwrap().unwrap();
        assert!(resolved.scopes.is_empty());
    }
}
