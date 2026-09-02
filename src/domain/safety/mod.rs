//! Safety alerts (01-data-model.md §7) — a deliberately simple,
//! always-available escape hatch, independent of the normal review flow.
//! Cross-cutting infrastructure (like `audit`): the safety alert path
//! itself is the one write a submissive can always make, and later
//! domains (check-ins, `13-checkins.md` §6) raise these automatically
//! rather than each reinventing the notion.

use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::audit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaisedVia {
    Submissive,
    // No caller yet: automatic escalation from a RED check-in
    // (13-checkins.md §6) is Phase 6 — the schema and this variant exist
    // now so that domain doesn't have to touch this module when it lands.
    #[allow(dead_code)]
    System,
}

impl RaisedVia {
    fn as_str(self) -> &'static str {
        match self {
            RaisedVia::Submissive => "submissive",
            RaisedVia::System => "system",
        }
    }
}

pub struct Raise<'a> {
    pub submissive_id: &'a str,
    pub link_id: &'a str,
    pub raised_via: RaisedVia,
    pub related_checkin_id: Option<&'a str>,
    pub message: Option<&'a str>,
}

/// Raises a new alert and writes the matching `audit_log` entry in the
/// same transaction — this is exactly the kind of action that must never
/// exist without a trace of who (or what) triggered it.
pub fn raise(conn: &mut Connection, raise: Raise) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::auth::session::now();

    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO safety_alerts
            (id, submissive_id, link_id, raised_at, raised_via, related_checkin_id, message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            raise.submissive_id,
            raise.link_id,
            now,
            raise.raised_via.as_str(),
            raise.related_checkin_id,
            raise.message,
        ],
    )?;

    let actor = match raise.raised_via {
        RaisedVia::Submissive => audit::Actor::User(raise.submissive_id),
        RaisedVia::System => audit::Actor::System,
    };
    audit::record(
        &tx,
        audit::Entry {
            actor,
            link_id: Some(raise.link_id),
            action: "safety_alert.raised",
            entity_type: "safety_alerts",
            entity_id: &id,
            detail: None,
        },
    )?;

    tx.commit()?;
    Ok(id)
}

/// Marks an alert as acknowledged — the Keyholder saw it and is responding.
pub fn acknowledge(
    conn: &Connection,
    alert_id: &str,
    acknowledged_by_user_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE safety_alerts SET acknowledged_at = ?1, acknowledged_by_user_id = ?2
         WHERE id = ?3 AND acknowledged_at IS NULL",
        params![
            crate::auth::session::now(),
            acknowledged_by_user_id,
            alert_id
        ],
    )?;
    Ok(())
}

pub struct Alert {
    pub id: String,
    pub submissive_id: String,
    pub link_id: String,
    pub raised_at: i64,
    pub raised_via: String,
    pub message: Option<String>,
    pub acknowledged_at: Option<i64>,
    pub acknowledged_by_user_id: Option<String>,
    pub resolved_at: Option<i64>,
}

const ALERT_COLUMNS: &str = "id, submissive_id, link_id, raised_at, raised_via, message, \
     acknowledged_at, acknowledged_by_user_id, resolved_at";

fn row_to_alert(row: &rusqlite::Row) -> rusqlite::Result<Alert> {
    Ok(Alert {
        id: row.get(0)?,
        submissive_id: row.get(1)?,
        link_id: row.get(2)?,
        raised_at: row.get(3)?,
        raised_via: row.get(4)?,
        message: row.get(5)?,
        acknowledged_at: row.get(6)?,
        acknowledged_by_user_id: row.get(7)?,
        resolved_at: row.get(8)?,
    })
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Alert>> {
    conn.query_row(
        &format!("SELECT {ALERT_COLUMNS} FROM safety_alerts WHERE id = ?1"),
        params![id],
        row_to_alert,
    )
    .optional()
}

/// Every alert across a Keyholder's own links (`GET
/// /keyholder/safety-alerts`, 03-api-design.md §5) — unresolved ones
/// first, newest first within each group, so the always-available escape
/// hatch is never buried under routine history.
pub fn list_for_links(conn: &Connection, link_ids: &[String]) -> rusqlite::Result<Vec<Alert>> {
    if link_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = link_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT {ALERT_COLUMNS} FROM safety_alerts
         WHERE link_id IN ({placeholders})
         ORDER BY (resolved_at IS NOT NULL) ASC, raised_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(rusqlite::params_from_iter(link_ids), row_to_alert)?
        .collect()
}

/// Marks an alert resolved — the situation is settled, not just seen.
/// Idempotent: resolving an already-resolved alert is a no-op rather
/// than an error, so a keyholder double-clicking doesn't need special
/// handling.
pub fn resolve(conn: &Connection, alert_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE safety_alerts SET resolved_at = ?1 WHERE id = ?2 AND resolved_at IS NULL",
        params![crate::auth::session::now(), alert_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_link(conn: &Connection) -> (String, String, String) {
        let keyholder_id = uuid::Uuid::new_v4().to_string();
        let submissive_id = uuid::Uuid::new_v4().to_string();
        let link_id = uuid::Uuid::new_v4().to_string();
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
        conn.execute(
            "INSERT INTO keyholder_submissive_links (id, keyholder_id, submissive_id, status, started_at)
             VALUES (?1, ?2, ?3, 'active', 0)",
            params![link_id, keyholder_id, submissive_id],
        )
        .unwrap();
        (keyholder_id, submissive_id, link_id)
    }

    #[test]
    fn raising_an_alert_also_writes_an_audit_log_entry() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let mut conn = pool.get().unwrap();
        let (_kh, sub, link) = seed_link(&conn);

        let alert_id = raise(
            &mut conn,
            Raise {
                submissive_id: &sub,
                link_id: &link,
                raised_via: RaisedVia::Submissive,
                related_checkin_id: None,
                message: Some("need to talk"),
            },
        )
        .unwrap();

        assert!(get(&conn, &alert_id).unwrap().is_some());

        let audit_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM audit_log WHERE entity_id = ?1 AND action = 'safety_alert.raised'",
                params![alert_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audit_count, 1);
    }

    #[test]
    fn system_raised_alert_has_no_human_actor() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let mut conn = pool.get().unwrap();
        let (_kh, sub, link) = seed_link(&conn);

        raise(
            &mut conn,
            Raise {
                submissive_id: &sub,
                link_id: &link,
                raised_via: RaisedVia::System,
                related_checkin_id: Some("checkin-1"),
                message: Some("auto-escalated from a RED check-in"),
            },
        )
        .unwrap();

        let actor_user_id: Option<String> = conn
            .query_row(
                "SELECT actor_user_id FROM audit_log WHERE action = 'safety_alert.raised'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(actor_user_id.is_none());
    }

    #[test]
    fn list_for_links_sorts_unresolved_before_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let mut conn = pool.get().unwrap();
        let (_kh, sub, link) = seed_link(&conn);

        let resolved = raise(
            &mut conn,
            Raise {
                submissive_id: &sub,
                link_id: &link,
                raised_via: RaisedVia::Submissive,
                related_checkin_id: None,
                message: None,
            },
        )
        .unwrap();
        let open = raise(
            &mut conn,
            Raise {
                submissive_id: &sub,
                link_id: &link,
                raised_via: RaisedVia::Submissive,
                related_checkin_id: None,
                message: None,
            },
        )
        .unwrap();
        resolve(&conn, &resolved).unwrap();

        let all = list_for_links(&conn, std::slice::from_ref(&link)).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, open);
        assert_eq!(all[1].id, resolved);
    }

    #[test]
    fn list_for_links_is_scoped_to_the_given_links() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let mut conn = pool.get().unwrap();
        let (_kh, sub, link) = seed_link(&conn);
        let (_kh2, sub2, other_link) = seed_link(&conn);

        raise(
            &mut conn,
            Raise {
                submissive_id: &sub,
                link_id: &link,
                raised_via: RaisedVia::Submissive,
                related_checkin_id: None,
                message: None,
            },
        )
        .unwrap();
        raise(
            &mut conn,
            Raise {
                submissive_id: &sub2,
                link_id: &other_link,
                raised_via: RaisedVia::Submissive,
                related_checkin_id: None,
                message: None,
            },
        )
        .unwrap();

        assert_eq!(list_for_links(&conn, &[link]).unwrap().len(), 1);
        assert_eq!(list_for_links(&conn, &[]).unwrap().len(), 0);
    }

    #[test]
    fn resolve_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let mut conn = pool.get().unwrap();
        let (_kh, sub, link) = seed_link(&conn);
        let alert_id = raise(
            &mut conn,
            Raise {
                submissive_id: &sub,
                link_id: &link,
                raised_via: RaisedVia::Submissive,
                related_checkin_id: None,
                message: None,
            },
        )
        .unwrap();

        resolve(&conn, &alert_id).unwrap();
        let first = get(&conn, &alert_id).unwrap().unwrap().resolved_at;
        resolve(&conn, &alert_id).unwrap();
        let second = get(&conn, &alert_id).unwrap().unwrap().resolved_at;
        assert_eq!(first, second);
    }

    #[test]
    fn acknowledging_sets_acknowledged_fields_once() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let mut conn = pool.get().unwrap();
        let (kh, sub, link) = seed_link(&conn);

        let alert_id = raise(
            &mut conn,
            Raise {
                submissive_id: &sub,
                link_id: &link,
                raised_via: RaisedVia::Submissive,
                related_checkin_id: None,
                message: None,
            },
        )
        .unwrap();

        acknowledge(&conn, &alert_id, &kh).unwrap();

        let acknowledged_at: Option<i64> = conn
            .query_row(
                "SELECT acknowledged_at FROM safety_alerts WHERE id = ?1",
                params![alert_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(acknowledged_at.is_some());
    }
}
