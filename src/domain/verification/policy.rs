//! `verification_policies` (01-data-model.md §5, 04-verification-workflow.md §1).

use rusqlite::{Connection, OptionalExtension, params};

pub struct Policy {
    pub link_id: String,
    pub frequency_kind: String,
    pub frequency_value: String,
    pub code_ttl_seconds: i64,
    pub grace_period_seconds: i64,
    pub updated_at: i64,
}

const COLUMNS: &str =
    "link_id, frequency_kind, frequency_value, code_ttl_seconds, grace_period_seconds, updated_at";

fn row_to_policy(row: &rusqlite::Row) -> rusqlite::Result<Policy> {
    Ok(Policy {
        link_id: row.get(0)?,
        frequency_kind: row.get(1)?,
        frequency_value: row.get(2)?,
        code_ttl_seconds: row.get(3)?,
        grace_period_seconds: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

pub fn get_for_link(conn: &Connection, link_id: &str) -> rusqlite::Result<Option<Policy>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM verification_policies WHERE link_id = ?1"),
        params![link_id],
        row_to_policy,
    )
    .optional()
}

pub struct SetPolicy<'a> {
    pub frequency_kind: &'a str,
    pub frequency_value: &'a str,
    pub code_ttl_seconds: i64,
    pub grace_period_seconds: i64,
}

/// Replaces the link's policy (`links::create` already seeded a default
/// one, so this is always an update of an existing row, never a fresh
/// insert — `PUT /keyholder/submissives/{id}/verification-policy`,
/// 03-api-design.md §5).
pub fn set_for_link(conn: &Connection, link_id: &str, policy: SetPolicy) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE verification_policies SET
            frequency_kind = ?1, frequency_value = ?2,
            code_ttl_seconds = ?3, grace_period_seconds = ?4, updated_at = ?5
         WHERE link_id = ?6",
        params![
            policy.frequency_kind,
            policy.frequency_value,
            policy.code_ttl_seconds,
            policy.grace_period_seconds,
            crate::auth::session::now(),
            link_id,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();

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
        let link_id = crate::domain::links::create(&conn, &keyholder_id, &submissive_id).unwrap();

        let default_policy = get_for_link(&conn, &link_id).unwrap().unwrap();
        assert_eq!(default_policy.frequency_kind, "on_demand_only");

        set_for_link(
            &conn,
            &link_id,
            SetPolicy {
                frequency_kind: "interval_hours",
                frequency_value: r#"{"hours":24}"#,
                code_ttl_seconds: 600,
                grace_period_seconds: 300,
            },
        )
        .unwrap();

        let updated = get_for_link(&conn, &link_id).unwrap().unwrap();
        assert_eq!(updated.frequency_kind, "interval_hours");
        assert_eq!(updated.code_ttl_seconds, 600);
    }
}
