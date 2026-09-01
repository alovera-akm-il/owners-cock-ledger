//! Append-only activity log (01-data-model.md §8) — every state-changing
//! action across every later domain writes one row here. Wired in now,
//! ahead of the first domain that needs it, so nothing has to retrofit
//! audit logging after the fact — which is also why nothing calls it yet
//! outside its own tests.
#![allow(dead_code)]

use rusqlite::{Connection, params};
use serde_json::Value;

/// A `NULL` actor is only unambiguous once you know *why* it's NULL — the
/// task deadline sweeper and an `admin` CLI command are both actor-less in
/// the `users.id` sense but for very different reasons (one is a schedule
/// firing, the other is a deliberate human decision made outside the app).
/// `Actor::System`/`Actor::AdminCli` record which, in `detail.actor_type`
/// (01-data-model.md §8).
pub enum Actor<'a> {
    User(&'a str),
    System,
    AdminCli,
}

pub struct Entry<'a> {
    pub actor: Actor<'a>,
    pub link_id: Option<&'a str>,
    pub action: &'a str,
    pub entity_type: &'a str,
    pub entity_id: &'a str,
    pub detail: Option<Value>,
}

pub fn record(conn: &Connection, entry: Entry) -> rusqlite::Result<()> {
    let (actor_user_id, actor_type_tag): (Option<&str>, Option<&str>) = match entry.actor {
        Actor::User(id) => (Some(id), None),
        Actor::System => (None, Some("system")),
        Actor::AdminCli => (None, Some("admin_cli")),
    };

    let detail = match (entry.detail, actor_type_tag) {
        (Some(Value::Object(mut map)), Some(tag)) => {
            map.insert("actor_type".to_string(), Value::String(tag.to_string()));
            Some(Value::Object(map))
        }
        (Some(other), Some(tag)) => {
            // Non-object detail from a system/admin actor still needs
            // actor_type recorded somewhere, so wrap it rather than
            // silently dropping the tag.
            let mut map = serde_json::Map::new();
            map.insert("actor_type".to_string(), Value::String(tag.to_string()));
            map.insert("value".to_string(), other);
            Some(Value::Object(map))
        }
        (None, Some(tag)) => {
            let mut map = serde_json::Map::new();
            map.insert("actor_type".to_string(), Value::String(tag.to_string()));
            Some(Value::Object(map))
        }
        (detail, None) => detail,
    };

    conn.execute(
        "INSERT INTO audit_log (id, actor_user_id, link_id, action, entity_type, entity_id, occurred_at, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            uuid::Uuid::new_v4().to_string(),
            actor_user_id,
            entry.link_id,
            entry.action,
            entry.entity_type,
            entry.entity_id,
            crate::auth::session::now(),
            detail.map(|v| v.to_string()),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool() -> (tempfile::TempDir, crate::db::Pool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        (dir, pool)
    }

    fn seed_user(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'Test User', 0)",
            params![id],
        )
        .unwrap();
    }

    #[test]
    fn user_actor_has_no_actor_type_tag() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        seed_user(&conn, "u1");
        record(
            &conn,
            Entry {
                actor: Actor::User("u1"),
                link_id: None,
                action: "profile.updated",
                entity_type: "users",
                entity_id: "u1",
                detail: None,
            },
        )
        .unwrap();

        let (actor_user_id, detail): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT actor_user_id, detail FROM audit_log LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(actor_user_id.as_deref(), Some("u1"));
        assert!(detail.is_none());
    }

    #[test]
    fn system_actor_tags_detail_with_actor_type() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        record(
            &conn,
            Entry {
                actor: Actor::System,
                link_id: None,
                action: "assignment.auto_failed",
                entity_type: "assignments",
                entity_id: "a1",
                detail: None,
            },
        )
        .unwrap();

        let (actor_user_id, detail): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT actor_user_id, detail FROM audit_log LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(actor_user_id.is_none());
        let detail: Value = serde_json::from_str(&detail.unwrap()).unwrap();
        assert_eq!(detail["actor_type"], "system");
    }

    #[test]
    fn admin_cli_actor_tags_detail_and_preserves_existing_fields() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        record(
            &conn,
            Entry {
                actor: Actor::AdminCli,
                link_id: None,
                action: "user.password_reset_forced",
                entity_type: "users",
                entity_id: "u2",
                detail: Some(serde_json::json!({"reason": "lost 2fa device"})),
            },
        )
        .unwrap();

        let detail: String = conn
            .query_row("SELECT detail FROM audit_log LIMIT 1", [], |row| row.get(0))
            .unwrap();
        let detail: Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(detail["actor_type"], "admin_cli");
        assert_eq!(detail["reason"], "lost 2fa device");
    }
}
