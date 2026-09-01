//! `keyholder_submissive_links` (01-data-model.md §3) — the join table
//! establishing ownership. Creating a link always creates its default
//! `verification_policies` row in the same transaction (01-data-model.md
//! §5), so there's never an undefined window before a Keyholder
//! configures a real schedule.

use rusqlite::{Connection, params};

const DEFAULT_CODE_TTL_SECS: i64 = 15 * 60;
const DEFAULT_GRACE_PERIOD_SECS: i64 = 10 * 60;

/// Creates an `active` link between a Keyholder and a submissive plus its
/// default on-demand-only verification policy. Callers are expected to run
/// this inside a transaction alongside whatever else the moment needs
/// (e.g. invite redemption also creates the submissive account).
pub fn create(
    conn: &Connection,
    keyholder_id: &str,
    submissive_id: &str,
) -> rusqlite::Result<String> {
    let link_id = uuid::Uuid::new_v4().to_string();
    let now = crate::auth::session::now();

    conn.execute(
        "INSERT INTO keyholder_submissive_links (id, keyholder_id, submissive_id, status, started_at)
         VALUES (?1, ?2, ?3, 'active', ?4)",
        params![link_id, keyholder_id, submissive_id, now],
    )?;

    conn.execute(
        "INSERT INTO verification_policies
            (id, link_id, frequency_kind, frequency_value, code_ttl_seconds, grace_period_seconds, created_at, updated_at)
         VALUES (?1, ?2, 'on_demand_only', '{}', ?3, ?4, ?5, ?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            link_id,
            DEFAULT_CODE_TTL_SECS,
            DEFAULT_GRACE_PERIOD_SECS,
            now,
        ],
    )?;

    Ok(link_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_users(conn: &Connection) -> (String, String) {
        let keyholder_id = uuid::Uuid::new_v4().to_string();
        let submissive_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH', 0)",
            params![keyholder_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Sub', 0)",
            params![submissive_id],
        )
        .unwrap();
        (keyholder_id, submissive_id)
    }

    #[test]
    fn creating_a_link_also_creates_its_default_verification_policy() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);

        let link_id = create(&conn, &kh, &sub).unwrap();

        let (frequency_kind, code_ttl): (String, i64) = conn
            .query_row(
                "SELECT frequency_kind, code_ttl_seconds FROM verification_policies WHERE link_id = ?1",
                params![link_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(frequency_kind, "on_demand_only");
        assert_eq!(code_ttl, DEFAULT_CODE_TTL_SECS);
    }

    #[test]
    fn a_submissive_cannot_have_two_active_links() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh1, sub) = seed_users(&conn);
        let kh2 = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH2', 0)",
            params![kh2],
        )
        .unwrap();

        create(&conn, &kh1, &sub).unwrap();
        let second = create(&conn, &kh2, &sub);
        assert!(second.is_err());
    }
}
