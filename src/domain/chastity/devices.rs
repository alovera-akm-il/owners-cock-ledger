//! `chastity_devices` (01-data-model.md §4).

use rusqlite::{Connection, params};

pub struct Device {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub added_at: i64,
    pub retired_at: Option<i64>,
}

pub fn add(
    conn: &Connection,
    submissive_id: &str,
    name: &str,
    description: Option<&str>,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO chastity_devices (id, submissive_id, name, description, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id,
            submissive_id,
            name,
            description,
            crate::auth::session::now()
        ],
    )?;
    Ok(id)
}

pub fn list(conn: &Connection, submissive_id: &str) -> rusqlite::Result<Vec<Device>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, added_at, retired_at
         FROM chastity_devices WHERE submissive_id = ?1
         ORDER BY added_at DESC",
    )?;
    stmt.query_map(params![submissive_id], |row| {
        Ok(Device {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            added_at: row.get(3)?,
            retired_at: row.get(4)?,
        })
    })?
    .collect()
}

/// Retires a device, scoped to its owning submissive. Returns `false` if
/// no matching row was found (wrong id, wrong owner).
pub fn retire(conn: &Connection, device_id: &str, submissive_id: &str) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "UPDATE chastity_devices SET retired_at = ?1
         WHERE id = ?2 AND submissive_id = ?3 AND retired_at IS NULL",
        params![crate::auth::session::now(), device_id, submissive_id],
    )?;
    Ok(affected > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool_with_submissive() -> (tempfile::TempDir, crate::db::Pool, String) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Sub', 0)",
            params![id],
        )
        .unwrap();
        (dir, pool, id)
    }

    #[test]
    fn add_then_list_round_trips() {
        let (_dir, pool, submissive_id) = temp_pool_with_submissive();
        let conn = pool.get().unwrap();
        add(&conn, &submissive_id, "steel #2", Some("daily wear")).unwrap();

        let devices = list(&conn, &submissive_id).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "steel #2");
        assert!(devices[0].retired_at.is_none());
    }

    #[test]
    fn retire_is_scoped_to_the_owning_submissive() {
        let (_dir, pool, submissive_id) = temp_pool_with_submissive();
        let conn = pool.get().unwrap();
        let device_id = add(&conn, &submissive_id, "steel #2", None).unwrap();

        let other_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Other', 0)",
            params![other_id],
        )
        .unwrap();

        assert!(!retire(&conn, &device_id, &other_id).unwrap());
        assert!(retire(&conn, &device_id, &submissive_id).unwrap());
        assert!(list(&conn, &submissive_id).unwrap()[0].retired_at.is_some());
    }
}
