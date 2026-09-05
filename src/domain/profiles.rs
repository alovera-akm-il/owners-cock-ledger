//! `keyholder_profiles`/`submissive_profiles` (01-data-model.md §2,
//! 03-api-design.md §3) — personal profile fields, distinct from account
//! credentials (email/password) and, for the Keyholder, API tokens.
//! Every account gets an empty row here at creation time
//! (`domain::users::create`), so a fetch never needs to handle "no
//! profile exists yet."

use rusqlite::{Connection, params};

pub struct KeyholderProfile {
    pub bio: Option<String>,
    pub contact_info: Option<String>,
    pub timezone: Option<String>,
    pub hard_limits: Option<String>,
    pub soft_limits: Option<String>,
    pub okay_limits: Option<String>,
}

pub fn get_keyholder_profile(
    conn: &Connection,
    user_id: &str,
) -> rusqlite::Result<KeyholderProfile> {
    conn.query_row(
        "SELECT bio, contact_info, timezone, hard_limits, soft_limits, okay_limits
         FROM keyholder_profiles WHERE user_id = ?1",
        params![user_id],
        |row| {
            Ok(KeyholderProfile {
                bio: row.get(0)?,
                contact_info: row.get(1)?,
                timezone: row.get(2)?,
                hard_limits: row.get(3)?,
                soft_limits: row.get(4)?,
                okay_limits: row.get(5)?,
            })
        },
    )
}

#[derive(Default)]
pub struct KeyholderProfileEdit<'a> {
    pub bio: Option<Option<&'a str>>,
    pub contact_info: Option<Option<&'a str>>,
    pub timezone: Option<Option<&'a str>>,
    pub hard_limits: Option<Option<&'a str>>,
    pub soft_limits: Option<Option<&'a str>>,
    pub okay_limits: Option<Option<&'a str>>,
}

/// A field left `None` here keeps its current value; `Some(None)`
/// explicitly clears it — the same double-`Option` PATCH shape used for
/// catalog template edits, for the same reason (a partial edit
/// shouldn't accidentally blank out fields it didn't mean to touch).
pub fn update_keyholder_profile(
    conn: &Connection,
    user_id: &str,
    edit: KeyholderProfileEdit,
) -> rusqlite::Result<()> {
    let current = get_keyholder_profile(conn, user_id)?;
    let bio = edit.bio.unwrap_or(current.bio.as_deref());
    let contact_info = edit.contact_info.unwrap_or(current.contact_info.as_deref());
    let timezone = edit.timezone.unwrap_or(current.timezone.as_deref());
    let hard_limits = edit.hard_limits.unwrap_or(current.hard_limits.as_deref());
    let soft_limits = edit.soft_limits.unwrap_or(current.soft_limits.as_deref());
    let okay_limits = edit.okay_limits.unwrap_or(current.okay_limits.as_deref());
    conn.execute(
        "UPDATE keyholder_profiles SET bio = ?1, contact_info = ?2, timezone = ?3,
            hard_limits = ?4, soft_limits = ?5, okay_limits = ?6 WHERE user_id = ?7",
        params![
            bio,
            contact_info,
            timezone,
            hard_limits,
            soft_limits,
            okay_limits,
            user_id
        ],
    )?;
    Ok(())
}

pub struct SubmissiveProfile {
    pub bio: Option<String>,
    pub safeword: Option<String>,
    pub hard_limits: Option<String>,
    pub soft_limits: Option<String>,
    pub okay_limits: Option<String>,
    pub emergency_contact: Option<String>,
    // Never returned from a submissive's own profile fetch
    // (01-data-model.md §2) — enforced by the API layer choosing a
    // response shape that omits this field, not by anything here.
    pub keyholder_notes: Option<String>,
    pub timezone: Option<String>,
}

pub fn get_submissive_profile(
    conn: &Connection,
    user_id: &str,
) -> rusqlite::Result<SubmissiveProfile> {
    conn.query_row(
        "SELECT bio, safeword, hard_limits, soft_limits, emergency_contact,
                keyholder_notes, timezone, okay_limits
         FROM submissive_profiles WHERE user_id = ?1",
        params![user_id],
        |row| {
            Ok(SubmissiveProfile {
                bio: row.get(0)?,
                safeword: row.get(1)?,
                hard_limits: row.get(2)?,
                soft_limits: row.get(3)?,
                emergency_contact: row.get(4)?,
                keyholder_notes: row.get(5)?,
                timezone: row.get(6)?,
                okay_limits: row.get(7)?,
            })
        },
    )
}

#[derive(Default)]
pub struct SubmissiveProfileEdit<'a> {
    pub bio: Option<Option<&'a str>>,
    pub safeword: Option<Option<&'a str>>,
    pub hard_limits: Option<Option<&'a str>>,
    pub soft_limits: Option<Option<&'a str>>,
    pub okay_limits: Option<Option<&'a str>>,
    pub emergency_contact: Option<Option<&'a str>>,
    pub timezone: Option<Option<&'a str>>,
}

/// The submissive's own editable fields — deliberately excludes
/// `keyholder_notes` at the type level, not just by convention; see
/// `update_submissive_keyholder_notes` for the one field only a
/// Keyholder can write (02-roles-and-permissions.md §2).
pub fn update_submissive_profile(
    conn: &Connection,
    user_id: &str,
    edit: SubmissiveProfileEdit,
) -> rusqlite::Result<()> {
    let current = get_submissive_profile(conn, user_id)?;
    let bio = edit.bio.unwrap_or(current.bio.as_deref());
    let safeword = edit.safeword.unwrap_or(current.safeword.as_deref());
    let hard_limits = edit.hard_limits.unwrap_or(current.hard_limits.as_deref());
    let soft_limits = edit.soft_limits.unwrap_or(current.soft_limits.as_deref());
    let okay_limits = edit.okay_limits.unwrap_or(current.okay_limits.as_deref());
    let emergency_contact = edit
        .emergency_contact
        .unwrap_or(current.emergency_contact.as_deref());
    let timezone = edit.timezone.unwrap_or(current.timezone.as_deref());
    conn.execute(
        "UPDATE submissive_profiles SET bio = ?1, safeword = ?2, hard_limits = ?3,
            soft_limits = ?4, emergency_contact = ?5, timezone = ?6, okay_limits = ?7 WHERE user_id = ?8",
        params![
            bio,
            safeword,
            hard_limits,
            soft_limits,
            emergency_contact,
            timezone,
            okay_limits,
            user_id
        ],
    )?;
    Ok(())
}

/// `PATCH /keyholder/submissives/{id}/profile/notes` — the one field a
/// Keyholder may write on a submissive's profile; everything else there
/// (`hard_limits`/`soft_limits`/`safeword`) stays submissive-owned,
/// read-only to the Keyholder (02-roles-and-permissions.md §2).
pub fn update_keyholder_notes(
    conn: &Connection,
    submissive_id: &str,
    keyholder_notes: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE submissive_profiles SET keyholder_notes = ?1 WHERE user_id = ?2",
        params![keyholder_notes, submissive_id],
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

    fn seed_keyholder(conn: &Connection) -> String {
        crate::domain::users::create_keyholder(
            conn,
            crate::domain::users::NewAccount {
                email: "kh@example.test",
                password_hash: "hash",
                display_name: "KH",
            },
        )
        .unwrap()
    }

    fn seed_submissive(conn: &Connection) -> String {
        crate::domain::users::create_submissive(
            conn,
            crate::domain::users::NewAccount {
                email: "sub@example.test",
                password_hash: "hash",
                display_name: "Sub",
            },
        )
        .unwrap()
    }

    #[test]
    fn a_fresh_keyholder_profile_is_empty_but_gettable() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let id = seed_keyholder(&conn);

        let profile = get_keyholder_profile(&conn, &id).unwrap();
        assert!(profile.bio.is_none());
    }

    #[test]
    fn update_keyholder_profile_only_touches_provided_fields() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let id = seed_keyholder(&conn);

        update_keyholder_profile(
            &conn,
            &id,
            KeyholderProfileEdit {
                bio: Some(Some("A strict but fair keyholder.")),
                timezone: Some(Some("America/New_York")),
                ..Default::default()
            },
        )
        .unwrap();

        update_keyholder_profile(
            &conn,
            &id,
            KeyholderProfileEdit {
                contact_info: Some(Some("signal: kh123")),
                ..Default::default()
            },
        )
        .unwrap();

        let profile = get_keyholder_profile(&conn, &id).unwrap();
        assert_eq!(profile.bio.as_deref(), Some("A strict but fair keyholder."));
        assert_eq!(profile.timezone.as_deref(), Some("America/New_York"));
        assert_eq!(profile.contact_info.as_deref(), Some("signal: kh123"));
    }

    #[test]
    fn update_keyholder_profile_can_explicitly_clear_a_field() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let id = seed_keyholder(&conn);
        update_keyholder_profile(
            &conn,
            &id,
            KeyholderProfileEdit {
                bio: Some(Some("temporary")),
                ..Default::default()
            },
        )
        .unwrap();

        update_keyholder_profile(
            &conn,
            &id,
            KeyholderProfileEdit {
                bio: Some(None),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(get_keyholder_profile(&conn, &id).unwrap().bio.is_none());
    }

    #[test]
    fn submissive_profile_edit_never_touches_keyholder_notes() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let id = seed_submissive(&conn);
        update_keyholder_notes(&conn, &id, Some("keeps missing verification windows")).unwrap();

        update_submissive_profile(
            &conn,
            &id,
            SubmissiveProfileEdit {
                bio: Some(Some("here to behave")),
                safeword: Some(Some("banana")),
                ..Default::default()
            },
        )
        .unwrap();

        let profile = get_submissive_profile(&conn, &id).unwrap();
        assert_eq!(profile.bio.as_deref(), Some("here to behave"));
        assert_eq!(profile.safeword.as_deref(), Some("banana"));
        assert_eq!(
            profile.keyholder_notes.as_deref(),
            Some("keeps missing verification windows")
        );
    }

    #[test]
    fn update_keyholder_notes_only_touches_that_field() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let id = seed_submissive(&conn);
        update_submissive_profile(
            &conn,
            &id,
            SubmissiveProfileEdit {
                bio: Some(Some("here to behave")),
                ..Default::default()
            },
        )
        .unwrap();

        update_keyholder_notes(&conn, &id, Some("doing well lately")).unwrap();

        let profile = get_submissive_profile(&conn, &id).unwrap();
        assert_eq!(profile.bio.as_deref(), Some("here to behave"));
        assert_eq!(
            profile.keyholder_notes.as_deref(),
            Some("doing well lately")
        );
    }
}
