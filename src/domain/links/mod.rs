//! `keyholder_submissive_links` (01-data-model.md §3) — the join table
//! establishing ownership. Creating a link always creates its default
//! `verification_policies` row in the same transaction (01-data-model.md
//! §5), so there's never an undefined window before a Keyholder
//! configures a real schedule.

use rusqlite::{Connection, OptionalExtension, params};

const DEFAULT_CODE_TTL_SECS: i64 = 15 * 60;
const DEFAULT_GRACE_PERIOD_SECS: i64 = 10 * 60;

/// The caller's own active link — every submissive-role query is
/// implicitly scoped to this (02-roles-and-permissions.md §1 principle
/// 3).
pub fn active_link_for_submissive(
    conn: &Connection,
    submissive_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM keyholder_submissive_links
         WHERE submissive_id = ?1 AND status = 'active'",
        params![submissive_id],
        |row| row.get(0),
    )
    .optional()
}

/// Every `active` link id for a Keyholder — the cross-roster feed's
/// scoping join (03-api-design.md §6, 02-roles-and-permissions.md §5).
pub fn active_link_ids_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM keyholder_submissive_links WHERE keyholder_id = ?1 AND status = 'active'",
    )?;
    stmt.query_map(params![keyholder_id], |row| row.get(0))?
        .collect()
}

/// Resolves `submissive_id` to the caller's own link to them, or `None`
/// if no such link exists — the join every Keyholder-role query must go
/// through rather than trusting a client-supplied submissive id
/// (02-roles-and-permissions.md §1 principle 2). Includes `paused` links
/// (a Keyholder still has read access to those, per that same principle)
/// but not `ended` ones, since none of Phase 2's write actions should be
/// reachable against a relationship that's over.
pub fn active_or_paused_link_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
    submissive_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM keyholder_submissive_links
         WHERE keyholder_id = ?1 AND submissive_id = ?2 AND status IN ('active', 'paused')",
        params![keyholder_id, submissive_id],
        |row| row.get(0),
    )
    .optional()
}

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

/// `admin force-end-link <link_id>` (10-operations.md §5,
/// 06-future-extensions.md §2) — the Tier 2 escape hatch for a Keyholder
/// who never responds to an end-link request at all. Ends an `active` or
/// `paused` link unilaterally; returns `false` if no such link exists to
/// end (already ended, or the id doesn't exist).
pub fn force_end(conn: &Connection, link_id: &str) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "UPDATE keyholder_submissive_links SET status = 'ended', ended_at = ?1
         WHERE id = ?2 AND status IN ('active', 'paused')",
        params![crate::auth::session::now(), link_id],
    )?;
    Ok(affected > 0)
}

#[derive(Debug, thiserror::Error)]
pub enum SetStatusError {
    #[error("link not found or not yours")]
    NotFound,
    #[error("that status transition isn't allowed from the link's current status")]
    InvalidTransition,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PATCH /keyholder/submissives/{id}/link` (03-api-design.md §2) —
/// only forward transitions (`active`→`paused`, `active`→`ended`,
/// `paused`→`ended`); there's no way back to `active` here on purpose
/// (a new invite starts a fresh link instead, 02-roles-and-permissions.md
/// §4). Keyholder-scoped, same as every other link mutation.
pub fn set_status(
    conn: &Connection,
    link_id: &str,
    keyholder_id: &str,
    new_status: &str,
) -> Result<(), SetStatusError> {
    let current: Option<String> = conn
        .query_row(
            "SELECT status FROM keyholder_submissive_links WHERE id = ?1 AND keyholder_id = ?2",
            params![link_id, keyholder_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current) = current else {
        return Err(SetStatusError::NotFound);
    };

    let allowed = matches!(
        (current.as_str(), new_status),
        ("active", "paused") | ("active", "ended") | ("paused", "ended")
    );
    if !allowed {
        return Err(SetStatusError::InvalidTransition);
    }

    if new_status == "ended" {
        conn.execute(
            "UPDATE keyholder_submissive_links SET status = ?1, ended_at = ?2 WHERE id = ?3",
            params![new_status, crate::auth::session::now(), link_id],
        )?;
    } else {
        conn.execute(
            "UPDATE keyholder_submissive_links SET status = ?1 WHERE id = ?2",
            params![new_status, link_id],
        )?;
    }
    Ok(())
}

pub struct LinkSettings {
    pub self_report_allowed: bool,
    pub catalog_visible_to_submissive: bool,
    /// Points are opt-in per link (01-data-model.md §12,
    /// 11-tasks-and-rewards.md §3) — folded into this same settings
    /// endpoint per 03-api-design.md §10c rather than a separate route.
    pub points_enabled: bool,
}

/// `PATCH /keyholder/submissives/{id}/link/settings` (03-api-design.md
/// §2, §10c). Returns `false` if no such link belongs to this Keyholder.
pub fn set_settings(
    conn: &Connection,
    link_id: &str,
    keyholder_id: &str,
    settings: LinkSettings,
) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "UPDATE keyholder_submissive_links
         SET self_report_allowed = ?1, catalog_visible_to_submissive = ?2, points_enabled = ?3
         WHERE id = ?4 AND keyholder_id = ?5",
        params![
            settings.self_report_allowed,
            settings.catalog_visible_to_submissive,
            settings.points_enabled,
            link_id,
            keyholder_id
        ],
    )?;
    Ok(affected > 0)
}

/// `(keyholder_id, submissive_id)` for a link — the lookup every
/// notification trigger needs to resolve "who's the other party" from
/// whichever id it already has on hand.
pub fn parties(conn: &Connection, link_id: &str) -> rusqlite::Result<Option<(String, String)>> {
    conn.query_row(
        "SELECT keyholder_id, submissive_id FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// Read side of the settings above — gates the submissive self-report
/// confinement endpoints (03-api-design.md §4) and catalog read access
/// (03-api-design.md §7).
pub fn settings_for_link(conn: &Connection, link_id: &str) -> rusqlite::Result<LinkSettings> {
    conn.query_row(
        "SELECT self_report_allowed, catalog_visible_to_submissive, points_enabled
         FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| {
            Ok(LinkSettings {
                self_report_allowed: row.get(0)?,
                catalog_visible_to_submissive: row.get(1)?,
                points_enabled: row.get(2)?,
            })
        },
    )
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

    #[test]
    fn force_end_ends_an_active_link_and_is_idempotent_false_after() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        assert!(force_end(&conn, &link_id).unwrap());
        let status: String = conn
            .query_row(
                "SELECT status FROM keyholder_submissive_links WHERE id = ?1",
                params![link_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "ended");

        assert!(!force_end(&conn, &link_id).unwrap());
    }

    #[test]
    fn set_status_allows_forward_transitions_only() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        set_status(&conn, &link_id, &kh, "paused").unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM keyholder_submissive_links WHERE id = ?1",
                params![link_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "paused");

        // Can't go back to active.
        let result = set_status(&conn, &link_id, &kh, "active");
        assert!(matches!(result, Err(SetStatusError::InvalidTransition)));

        set_status(&conn, &link_id, &kh, "ended").unwrap();
        let (status, ended_at): (String, Option<i64>) = conn
            .query_row(
                "SELECT status, ended_at FROM keyholder_submissive_links WHERE id = ?1",
                params![link_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "ended");
        assert!(ended_at.is_some());

        // Nothing is a valid transition out of ended.
        let result = set_status(&conn, &link_id, &kh, "paused");
        assert!(matches!(result, Err(SetStatusError::InvalidTransition)));
    }

    #[test]
    fn set_status_is_scoped_to_the_owning_keyholder() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        let result = set_status(&conn, &link_id, "someone-else", "paused");
        assert!(matches!(result, Err(SetStatusError::NotFound)));
    }

    #[test]
    fn parties_resolves_both_ids_and_none_for_an_unknown_link() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        assert_eq!(parties(&conn, &link_id).unwrap(), Some((kh, sub)));
        assert_eq!(parties(&conn, "no-such-link").unwrap(), None);
    }

    #[test]
    fn settings_default_to_self_report_off_and_catalog_visible() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        let settings = settings_for_link(&conn, &link_id).unwrap();
        assert!(!settings.self_report_allowed);
        assert!(settings.catalog_visible_to_submissive);
    }

    #[test]
    fn set_settings_updates_both_flags_and_is_scoped_to_the_keyholder() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        assert!(
            !set_settings(
                &conn,
                &link_id,
                "someone-else",
                LinkSettings {
                    self_report_allowed: true,
                    catalog_visible_to_submissive: false,
                    points_enabled: false,
                },
            )
            .unwrap()
        );

        assert!(
            set_settings(
                &conn,
                &link_id,
                &kh,
                LinkSettings {
                    self_report_allowed: true,
                    catalog_visible_to_submissive: false,
                    points_enabled: false,
                },
            )
            .unwrap()
        );

        let settings = settings_for_link(&conn, &link_id).unwrap();
        assert!(settings.self_report_allowed);
        assert!(!settings.catalog_visible_to_submissive);
    }
}
