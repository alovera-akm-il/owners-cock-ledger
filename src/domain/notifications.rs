//! In-app notification feed (01-data-model.md §10, 09-notifications.md).
//! The durable record every trigger writes to — exists independent of
//! whether Web Push delivery succeeds, is enabled, or is even
//! attempted, so this module has no knowledge of push at all (see
//! `domain::push` for that half). Cross-cutting infrastructure like
//! `audit`/`safety`: every later domain that fires a notification calls
//! straight into here rather than reinventing storage.

use rusqlite::{Connection, OptionalExtension, params};

pub struct NewNotification<'a> {
    pub user_id: &'a str,
    pub link_id: Option<&'a str>,
    pub notification_type: &'a str,
    pub title: &'a str,
    pub body: Option<&'a str>,
    pub link_path: Option<&'a str>,
    pub related_entity_type: Option<&'a str>,
    pub related_entity_id: Option<&'a str>,
}

#[derive(Clone)]
pub struct Notification {
    pub id: String,
    pub user_id: String,
    // Full mirror of the `notifications` row (01-data-model.md §10);
    // not every field is read back out by the API layer today
    // (`link_path` already carries the deep-link a client needs), but
    // these three are what future consumers (email digests, per-type
    // preferences, 09-notifications.md §6) would key off of.
    #[allow(dead_code)]
    pub link_id: Option<String>,
    pub notification_type: String,
    pub title: String,
    pub body: Option<String>,
    pub link_path: Option<String>,
    #[allow(dead_code)]
    pub related_entity_type: Option<String>,
    #[allow(dead_code)]
    pub related_entity_id: Option<String>,
    pub created_at: i64,
    pub read_at: Option<i64>,
}

const NOTIFICATION_COLUMNS: &str = "id, user_id, link_id, type, title, body, link_path, \
     related_entity_type, related_entity_id, created_at, read_at";

fn row_to_notification(row: &rusqlite::Row) -> rusqlite::Result<Notification> {
    Ok(Notification {
        id: row.get(0)?,
        user_id: row.get(1)?,
        link_id: row.get(2)?,
        notification_type: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        link_path: row.get(6)?,
        related_entity_type: row.get(7)?,
        related_entity_id: row.get(8)?,
        created_at: row.get(9)?,
        read_at: row.get(10)?,
    })
}

/// Writes the durable record. Callers that also want a push attempt
/// wrap this with `crate::notify::notify` (async, since push delivery
/// is a network call) rather than this module knowing anything about
/// delivery mechanics.
pub fn create(conn: &Connection, new: NewNotification) -> rusqlite::Result<Notification> {
    let id = uuid::Uuid::new_v4().to_string();
    let created_at = crate::auth::session::now();
    conn.execute(
        "INSERT INTO notifications
            (id, user_id, link_id, type, title, body, link_path, related_entity_type, related_entity_id, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            id,
            new.user_id,
            new.link_id,
            new.notification_type,
            new.title,
            new.body,
            new.link_path,
            new.related_entity_type,
            new.related_entity_id,
            created_at,
        ],
    )?;
    Ok(Notification {
        id,
        user_id: new.user_id.to_string(),
        link_id: new.link_id.map(str::to_string),
        notification_type: new.notification_type.to_string(),
        title: new.title.to_string(),
        body: new.body.map(str::to_string),
        link_path: new.link_path.map(str::to_string),
        related_entity_type: new.related_entity_type.map(str::to_string),
        related_entity_id: new.related_entity_id.map(str::to_string),
        created_at,
        read_at: None,
    })
}

/// Dedupe check for the deadline sweeper's reminder pass
/// (`08-punishments-and-deadlines.md` §3 step 2) — "has this assignment
/// already gotten its one `task.deadline_approaching` notification,"
/// checked against `notifications` itself rather than a new column on
/// `assignments`, per that section's own reasoning.
pub fn exists_for_related_entity(
    conn: &Connection,
    notification_type: &str,
    related_entity_id: &str,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM notifications WHERE type = ?1 AND related_entity_id = ?2 LIMIT 1",
        params![notification_type, related_entity_id],
        |_| Ok(()),
    )
    .optional()
    .map(|r| r.is_some())
}

/// Same dedupe idea, windowed — backs the still-paused reminder
/// (`08-punishments-and-deadlines.md` §9), which is meant to repeat
/// roughly daily rather than fire once ever.
pub fn exists_for_related_entity_since(
    conn: &Connection,
    notification_type: &str,
    related_entity_id: &str,
    since: i64,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM notifications WHERE type = ?1 AND related_entity_id = ?2 AND created_at >= ?3 LIMIT 1",
        params![notification_type, related_entity_id, since],
        |_| Ok(()),
    )
    .optional()
    .map(|r| r.is_some())
}

/// `GET /notifications` (03-api-design.md §13) — newest first. No
/// cursor pagination: consistent with every other list endpoint in
/// this codebase (proof submissions, audit log, assignments, safety
/// alerts), none of which built the cursor convention documented in
/// §11 either — a bounded most-recent window is the established
/// pattern at this scale.
pub fn list_for_user(
    conn: &Connection,
    user_id: &str,
    unread_only: bool,
    limit: i64,
) -> rusqlite::Result<Vec<Notification>> {
    let sql = format!(
        "SELECT {NOTIFICATION_COLUMNS} FROM notifications
         WHERE user_id = ?1 {}
         ORDER BY created_at DESC, id DESC
         LIMIT ?2",
        if unread_only {
            "AND read_at IS NULL"
        } else {
            ""
        }
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![user_id, limit], row_to_notification)?
        .collect()
}

/// Marks one notification read. Returns `false` if no row matched
/// (wrong id, or not owned by `user_id`) so the API layer can 404 —
/// idempotent otherwise, marking an already-read notification read
/// again is a no-op success, not an error.
pub fn mark_read(conn: &Connection, id: &str, user_id: &str) -> rusqlite::Result<bool> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM notifications WHERE id = ?1 AND user_id = ?2",
            params![id, user_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        return Ok(false);
    }
    conn.execute(
        "UPDATE notifications SET read_at = ?1 WHERE id = ?2 AND read_at IS NULL",
        params![crate::auth::session::now(), id],
    )?;
    Ok(true)
}

pub fn mark_all_read(conn: &Connection, user_id: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE notifications SET read_at = ?1 WHERE user_id = ?2 AND read_at IS NULL",
        params![crate::auth::session::now(), user_id],
    )
}

pub fn mark_push_dispatched(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE notifications SET push_dispatched_at = ?1 WHERE id = ?2",
        params![crate::auth::session::now(), id],
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

    fn seed_user(conn: &Connection) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Sub', 0)",
            params![id],
        )
        .unwrap();
        id
    }

    fn new_notification<'a>(user_id: &'a str, notification_type: &'a str) -> NewNotification<'a> {
        NewNotification {
            user_id,
            link_id: None,
            notification_type,
            title: "Something happened",
            body: None,
            link_path: None,
            related_entity_type: None,
            related_entity_id: None,
        }
    }

    #[test]
    fn create_and_list_round_trips() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);

        create(&conn, new_notification(&user, "safety.alert_raised")).unwrap();
        let list = list_for_user(&conn, &user, false, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].notification_type, "safety.alert_raised");
        assert!(list[0].read_at.is_none());
    }

    #[test]
    fn list_is_scoped_to_the_user_and_newest_first() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);
        let other = seed_user(&conn);

        create(&conn, new_notification(&user, "a")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let second = create(&conn, new_notification(&user, "b")).unwrap();
        create(&conn, new_notification(&other, "c")).unwrap();

        let list = list_for_user(&conn, &user, false, 10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second.id);
    }

    #[test]
    fn unread_only_filter_excludes_read_notifications() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);

        let n = create(&conn, new_notification(&user, "a")).unwrap();
        create(&conn, new_notification(&user, "b")).unwrap();
        mark_read(&conn, &n.id, &user).unwrap();

        let unread = list_for_user(&conn, &user, true, 10).unwrap();
        assert_eq!(unread.len(), 1);
    }

    #[test]
    fn mark_read_returns_false_for_someone_elses_notification() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);
        let other = seed_user(&conn);

        let n = create(&conn, new_notification(&user, "a")).unwrap();
        assert!(!mark_read(&conn, &n.id, &other).unwrap());
        assert!(mark_read(&conn, &n.id, &user).unwrap());
    }

    #[test]
    fn mark_all_read_only_touches_the_given_user() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);
        let other = seed_user(&conn);

        create(&conn, new_notification(&user, "a")).unwrap();
        create(&conn, new_notification(&user, "b")).unwrap();
        create(&conn, new_notification(&other, "c")).unwrap();

        let updated = mark_all_read(&conn, &user).unwrap();
        assert_eq!(updated, 2);
        assert_eq!(list_for_user(&conn, &user, true, 10).unwrap().len(), 0);
        assert_eq!(list_for_user(&conn, &other, true, 10).unwrap().len(), 1);
    }

    #[test]
    fn exists_for_related_entity_dedupes_reminders() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);

        assert!(
            !exists_for_related_entity(&conn, "task.deadline_approaching", "assignment-1").unwrap()
        );
        conn.execute(
            "INSERT INTO notifications (id, user_id, type, title, related_entity_id, created_at)
             VALUES ('n1', ?1, 'task.deadline_approaching', 'x', 'assignment-1', 0)",
            params![user],
        )
        .unwrap();
        assert!(
            exists_for_related_entity(&conn, "task.deadline_approaching", "assignment-1").unwrap()
        );
    }

    #[test]
    fn exists_for_related_entity_since_respects_the_window() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);
        conn.execute(
            "INSERT INTO notifications (id, user_id, type, title, related_entity_id, created_at)
             VALUES ('n1', ?1, 'confinement.clock_still_paused', 'x', 'session-1', 1000)",
            params![user],
        )
        .unwrap();

        assert!(
            exists_for_related_entity_since(
                &conn,
                "confinement.clock_still_paused",
                "session-1",
                500
            )
            .unwrap()
        );
        assert!(
            !exists_for_related_entity_since(
                &conn,
                "confinement.clock_still_paused",
                "session-1",
                1500
            )
            .unwrap()
        );
    }
}
