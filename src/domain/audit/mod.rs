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

pub struct LogRow {
    pub id: String,
    pub actor_user_id: Option<String>,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub occurred_at: i64,
    pub detail: Option<String>,
    /// Resolved via `link_id` for most actions, or via `toys.submissive_id`
    /// for the toy actions that don't carry a `link_id` — `None` for rows
    /// that are about the keyholder's own account (an admin-CLI action
    /// against them, say) rather than any one submissive.
    pub submissive_id: Option<String>,
    pub submissive_display_name: Option<String>,
    /// `None` when `actor_user_id` is `None` (a system tick or admin-CLI
    /// command) — the caller distinguishes those via `detail`'s
    /// `actor_type` tag, same as `record()` writes it.
    pub actor_display_name: Option<String>,
}

/// Every row relevant to one Keyholder: everything scoped to one of
/// their own links, anything they personally actioned (even where
/// `link_id` isn't set, e.g. retiring a toy), and admin-CLI rows
/// about their own account. Resolves the submissive/actor names in
/// the same query rather than a separate round trip per row.
pub fn list_for_keyholder(conn: &Connection, keyholder_id: &str) -> rusqlite::Result<Vec<LogRow>> {
    let mut stmt = conn.prepare(
        "SELECT al.id, al.actor_user_id, al.action, al.entity_type, al.entity_id,
                al.occurred_at, al.detail,
                COALESCE(link_sub.id, toy_sub.id),
                COALESCE(link_sub.display_name, toy_sub.display_name),
                actor.display_name
         FROM audit_log al
         LEFT JOIN keyholder_submissive_links kl ON kl.id = al.link_id
         LEFT JOIN users link_sub ON link_sub.id = kl.submissive_id
         LEFT JOIN toys t ON al.entity_type = 'toys' AND t.id = al.entity_id
         LEFT JOIN users toy_sub ON toy_sub.id = t.submissive_id
         LEFT JOIN users actor ON actor.id = al.actor_user_id
         WHERE al.link_id IN (SELECT id FROM keyholder_submissive_links WHERE keyholder_id = ?1)
            OR al.actor_user_id = ?1
            OR (al.entity_type = 'users' AND al.entity_id = ?1)
         ORDER BY al.occurred_at DESC",
    )?;
    stmt.query_map(params![keyholder_id], |row| {
        Ok(LogRow {
            id: row.get(0)?,
            actor_user_id: row.get(1)?,
            action: row.get(2)?,
            entity_type: row.get(3)?,
            entity_id: row.get(4)?,
            occurred_at: row.get(5)?,
            detail: row.get(6)?,
            submissive_id: row.get(7)?,
            submissive_display_name: row.get(8)?,
            actor_display_name: row.get(9)?,
        })
    })?
    .collect()
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

    fn seed_link(conn: &Connection) -> (String, String, String) {
        let kh = crate::domain::users::create_keyholder(
            conn,
            crate::domain::users::NewAccount {
                email: "kh@example.test",
                password_hash: "hash",
                display_name: "KH",
            },
        )
        .unwrap();
        let sub = crate::domain::users::create_submissive(
            conn,
            crate::domain::users::NewAccount {
                email: "sub@example.test",
                password_hash: "hash",
                display_name: "Sub",
            },
        )
        .unwrap();
        let link_id = crate::domain::links::create(conn, &kh, &sub).unwrap();
        (kh, sub, link_id)
    }

    #[test]
    fn list_for_keyholder_includes_link_scoped_rows() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, sub, link_id) = seed_link(&conn);

        record(
            &conn,
            Entry {
                actor: Actor::User(&sub),
                link_id: Some(&link_id),
                action: "invite.redeemed",
                entity_type: "invites",
                entity_id: "i1",
                detail: None,
            },
        )
        .unwrap();

        let rows = list_for_keyholder(&conn, &kh).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "invite.redeemed");
        assert_eq!(rows[0].submissive_id.as_deref(), Some(sub.as_str()));
        assert_eq!(rows[0].submissive_display_name.as_deref(), Some("Sub"));
        assert_eq!(rows[0].actor_display_name.as_deref(), Some("Sub"));

        // A different Keyholder's own audit view never sees this row.
        let other_kh = crate::domain::users::create_keyholder(
            &conn,
            crate::domain::users::NewAccount {
                email: "kh2@example.test",
                password_hash: "hash",
                display_name: "KH2",
            },
        )
        .unwrap();
        assert!(list_for_keyholder(&conn, &other_kh).unwrap().is_empty());
    }

    #[test]
    fn list_for_keyholder_resolves_toy_actions_with_no_link_id_via_the_toy_row() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, sub, _link_id) = seed_link(&conn);
        conn.execute(
            "INSERT INTO toys (id, submissive_id, added_by_user_id, name)
             VALUES ('t1', ?1, ?1, 'steel cage')",
            params![sub],
        )
        .unwrap();

        record(
            &conn,
            Entry {
                actor: Actor::User(&kh),
                link_id: None,
                action: "toy.retired",
                entity_type: "toys",
                entity_id: "t1",
                detail: None,
            },
        )
        .unwrap();

        let rows = list_for_keyholder(&conn, &kh).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].submissive_id.as_deref(), Some(sub.as_str()));
        assert_eq!(rows[0].actor_display_name.as_deref(), Some("KH"));
    }

    #[test]
    fn list_for_keyholder_includes_admin_cli_rows_about_their_own_account() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, _sub, _link_id) = seed_link(&conn);

        record(
            &conn,
            Entry {
                actor: Actor::AdminCli,
                link_id: None,
                action: "user.password_reset_issued_via_admin_cli",
                entity_type: "users",
                entity_id: &kh,
                detail: None,
            },
        )
        .unwrap();

        let rows = list_for_keyholder(&conn, &kh).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].actor_user_id.is_none());
        assert!(rows[0].submissive_id.is_none());
    }
}
