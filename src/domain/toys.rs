//! Toy catalog (01-data-model.md §13, 12-toy-catalog.md) — per-submissive,
//! not a Keyholder-reusable template. Either role may add or edit;
//! retiring (the actual soft-delete) is Keyholder-only — a submissive
//! can only request removal (§3), which the Keyholder approves
//! (retires) or declines (clears the request).

use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::audit;

pub struct Toy {
    pub id: String,
    pub submissive_id: String,
    pub added_by_user_id: String,
    pub name: String,
    pub category: Option<String>,
    pub material: Option<String>,
    pub brand: Option<String>,
    pub size_notes: Option<String>,
    pub color: Option<String>,
    pub compatible_device_id: Option<String>,
    pub storage_location: Option<String>,
    pub care_instructions: Option<String>,
    pub usage_notes: Option<String>,
    pub tags: Option<String>,
    pub photo_attachment_path: Option<String>,
    pub photo_mime_type: Option<String>,
    pub acquired_at: Option<i64>,
    pub retirement_requested_at: Option<i64>,
    pub retired_at: Option<i64>,
    pub retired_by_user_id: Option<String>,
}

const COLUMNS: &str = "id, submissive_id, added_by_user_id, name, category, material, brand, \
     size_notes, color, compatible_device_id, storage_location, care_instructions, usage_notes, \
     tags, photo_attachment_path, photo_mime_type, acquired_at, retirement_requested_at, \
     retired_at, retired_by_user_id";

fn row_to_toy(row: &rusqlite::Row) -> rusqlite::Result<Toy> {
    Ok(Toy {
        id: row.get(0)?,
        submissive_id: row.get(1)?,
        added_by_user_id: row.get(2)?,
        name: row.get(3)?,
        category: row.get(4)?,
        material: row.get(5)?,
        brand: row.get(6)?,
        size_notes: row.get(7)?,
        color: row.get(8)?,
        compatible_device_id: row.get(9)?,
        storage_location: row.get(10)?,
        care_instructions: row.get(11)?,
        usage_notes: row.get(12)?,
        tags: row.get(13)?,
        photo_attachment_path: row.get(14)?,
        photo_mime_type: row.get(15)?,
        acquired_at: row.get(16)?,
        retirement_requested_at: row.get(17)?,
        retired_at: row.get(18)?,
        retired_by_user_id: row.get(19)?,
    })
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Toy>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM toys WHERE id = ?1"),
        params![id],
        row_to_toy,
    )
    .optional()
}

/// `GET .../toys` — excludes retired unless `include_retired`.
pub fn list_for_submissive(
    conn: &Connection,
    submissive_id: &str,
    include_retired: bool,
) -> rusqlite::Result<Vec<Toy>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM toys WHERE submissive_id = ?1 {} ORDER BY acquired_at DESC, name ASC",
        if include_retired {
            ""
        } else {
            "AND retired_at IS NULL"
        }
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![submissive_id], row_to_toy)?
        .collect()
}

pub struct NewToy<'a> {
    pub submissive_id: &'a str,
    pub added_by_user_id: &'a str,
    pub name: &'a str,
    pub category: Option<&'a str>,
    pub material: Option<&'a str>,
    pub brand: Option<&'a str>,
    pub size_notes: Option<&'a str>,
    pub color: Option<&'a str>,
    pub compatible_device_id: Option<&'a str>,
    pub storage_location: Option<&'a str>,
    pub care_instructions: Option<&'a str>,
    pub usage_notes: Option<&'a str>,
    pub tags: Option<&'a str>,
    pub photo_attachment_path: Option<&'a str>,
    pub acquired_at: Option<i64>,
}

/// `POST .../toys` — either role may create (12-toy-catalog.md §3).
pub fn create(conn: &Connection, new: NewToy) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO toys
            (id, submissive_id, added_by_user_id, name, category, material, brand, size_notes,
             color, compatible_device_id, storage_location, care_instructions, usage_notes, tags,
             photo_attachment_path, acquired_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        params![
            id,
            new.submissive_id,
            new.added_by_user_id,
            new.name,
            new.category,
            new.material,
            new.brand,
            new.size_notes,
            new.color,
            new.compatible_device_id,
            new.storage_location,
            new.care_instructions,
            new.usage_notes,
            new.tags,
            new.photo_attachment_path,
            new.acquired_at,
        ],
    )?;
    Ok(id)
}

/// Field left `None` keeps its current value; `Some(None)` clears it —
/// same double-`Option` PATCH shape used elsewhere in this codebase.
#[derive(Default)]
pub struct ToyEdit<'a> {
    pub name: Option<&'a str>,
    pub category: Option<Option<&'a str>>,
    pub material: Option<Option<&'a str>>,
    pub brand: Option<Option<&'a str>>,
    pub size_notes: Option<Option<&'a str>>,
    pub color: Option<Option<&'a str>>,
    pub compatible_device_id: Option<Option<&'a str>>,
    pub storage_location: Option<Option<&'a str>>,
    pub care_instructions: Option<Option<&'a str>>,
    pub usage_notes: Option<Option<&'a str>>,
    pub tags: Option<Option<&'a str>>,
    pub photo_attachment_path: Option<Option<&'a str>>,
    pub photo_mime_type: Option<Option<&'a str>>,
    pub acquired_at: Option<Option<i64>>,
}

/// `PATCH .../toys/{id}` — any field except `retired_at`/
/// `retired_by_user_id` (12-toy-catalog.md §4: "editing care notes,
/// tags, etc. doesn't need extra gating" for either role, on a toy
/// they can view). Returns `false` if no such toy exists.
pub fn update(conn: &Connection, id: &str, edit: ToyEdit) -> rusqlite::Result<bool> {
    let Some(current) = get(conn, id)? else {
        return Ok(false);
    };
    let name = edit.name.unwrap_or(&current.name);
    let category = edit.category.unwrap_or(current.category.as_deref());
    let material = edit.material.unwrap_or(current.material.as_deref());
    let brand = edit.brand.unwrap_or(current.brand.as_deref());
    let size_notes = edit.size_notes.unwrap_or(current.size_notes.as_deref());
    let color = edit.color.unwrap_or(current.color.as_deref());
    let compatible_device_id = edit
        .compatible_device_id
        .unwrap_or(current.compatible_device_id.as_deref());
    let storage_location = edit
        .storage_location
        .unwrap_or(current.storage_location.as_deref());
    let care_instructions = edit
        .care_instructions
        .unwrap_or(current.care_instructions.as_deref());
    let usage_notes = edit.usage_notes.unwrap_or(current.usage_notes.as_deref());
    let tags = edit.tags.unwrap_or(current.tags.as_deref());
    let photo_attachment_path = edit
        .photo_attachment_path
        .unwrap_or(current.photo_attachment_path.as_deref());
    let photo_mime_type = edit
        .photo_mime_type
        .unwrap_or(current.photo_mime_type.as_deref());
    let acquired_at = edit.acquired_at.unwrap_or(current.acquired_at);

    conn.execute(
        "UPDATE toys SET name = ?1, category = ?2, material = ?3, brand = ?4, size_notes = ?5,
            color = ?6, compatible_device_id = ?7, storage_location = ?8, care_instructions = ?9,
            usage_notes = ?10, tags = ?11, photo_attachment_path = ?12, photo_mime_type = ?13,
            acquired_at = ?14
         WHERE id = ?15",
        params![
            name,
            category,
            material,
            brand,
            size_notes,
            color,
            compatible_device_id,
            storage_location,
            care_instructions,
            usage_notes,
            tags,
            photo_attachment_path,
            photo_mime_type,
            acquired_at,
            id,
        ],
    )?;
    Ok(true)
}

#[derive(Debug, thiserror::Error)]
pub enum RequestRemovalError {
    #[error("toy not found")]
    NotFound,
    #[error("already retired, or a removal request is already pending")]
    Conflict,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST /submissive/toys/{id}/request-removal` — flags the toy as
/// pending-removal; it stays fully visible and usable until the
/// Keyholder acts on it.
pub fn request_removal(conn: &Connection, id: &str) -> Result<(), RequestRemovalError> {
    let toy = get(conn, id)?.ok_or(RequestRemovalError::NotFound)?;
    if toy.retired_at.is_some() || toy.retirement_requested_at.is_some() {
        return Err(RequestRemovalError::Conflict);
    }
    conn.execute(
        "UPDATE toys SET retirement_requested_at = ?1 WHERE id = ?2",
        params![crate::auth::session::now(), id],
    )?;
    Ok(())
}

/// `POST /keyholder/toys/{id}/retire` — the actual soft-delete.
/// Clears any pending request implicitly (it's now moot). A Keyholder
/// can retire directly, with no prior request.
pub fn retire(conn: &mut Connection, id: &str, retired_by_user_id: &str) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE toys SET retired_at = ?1, retired_by_user_id = ?2, retirement_requested_at = NULL
         WHERE id = ?3",
        params![crate::auth::session::now(), retired_by_user_id, id],
    )?;
    audit::record(
        &tx,
        audit::Entry {
            actor: audit::Actor::User(retired_by_user_id),
            link_id: None,
            action: "toy.retired",
            entity_type: "toys",
            entity_id: id,
            detail: None,
        },
    )?;
    tx.commit()
}

#[derive(Debug, thiserror::Error)]
pub enum DeclineRemovalError {
    #[error("no removal request is pending")]
    NotPending,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST /keyholder/toys/{id}/decline-removal` — clears the request
/// without retiring; audit-logged so the decline itself is recorded
/// (12-toy-catalog.md §3).
pub fn decline_removal(
    conn: &mut Connection,
    id: &str,
    declined_by_user_id: &str,
) -> Result<(), DeclineRemovalError> {
    let tx = conn.transaction()?;
    let affected = tx.execute(
        "UPDATE toys SET retirement_requested_at = NULL
         WHERE id = ?1 AND retirement_requested_at IS NOT NULL",
        params![id],
    )?;
    if affected == 0 {
        return Err(DeclineRemovalError::NotPending);
    }
    audit::record(
        &tx,
        audit::Entry {
            actor: audit::Actor::User(declined_by_user_id),
            link_id: None,
            action: "toy.removal_declined",
            entity_type: "toys",
            entity_id: id,
            detail: None,
        },
    )?;
    tx.commit()?;
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

    fn new_toy<'a>(submissive_id: &'a str, added_by: &'a str, name: &'a str) -> NewToy<'a> {
        NewToy {
            submissive_id,
            added_by_user_id: added_by,
            name,
            category: None,
            material: None,
            brand: None,
            size_notes: None,
            color: None,
            compatible_device_id: None,
            storage_location: None,
            care_instructions: None,
            usage_notes: None,
            tags: None,
            photo_attachment_path: None,
            acquired_at: None,
        }
    }

    #[test]
    fn create_and_list_round_trips_and_excludes_retired_by_default() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);

        let id = create(&conn, new_toy(&sub, &kh, "steel cage")).unwrap();
        let list = list_for_submissive(&conn, &sub, false).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "steel cage");

        let mut conn_mut = pool.get().unwrap();
        retire(&mut conn_mut, &id, &kh).unwrap();

        assert_eq!(list_for_submissive(&conn, &sub, false).unwrap().len(), 0);
        assert_eq!(list_for_submissive(&conn, &sub, true).unwrap().len(), 1);
    }

    #[test]
    fn update_only_touches_provided_fields_and_supports_clearing() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let id = create(&conn, new_toy(&sub, &kh, "bullet vibe")).unwrap();

        update(
            &conn,
            &id,
            ToyEdit {
                category: Some(Some("vibrator")),
                tags: Some(Some("[\"quiet\"]")),
                ..Default::default()
            },
        )
        .unwrap();
        let t = get(&conn, &id).unwrap().unwrap();
        assert_eq!(t.name, "bullet vibe");
        assert_eq!(t.category.as_deref(), Some("vibrator"));

        update(
            &conn,
            &id,
            ToyEdit {
                category: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(get(&conn, &id).unwrap().unwrap().category.is_none());
    }

    #[test]
    fn request_removal_then_retire_clears_the_pending_flag() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let id = create(&conn, new_toy(&sub, &sub, "nylon rope")).unwrap();

        request_removal(&conn, &id).unwrap();
        assert!(
            get(&conn, &id)
                .unwrap()
                .unwrap()
                .retirement_requested_at
                .is_some()
        );

        // Can't double-request.
        assert!(matches!(
            request_removal(&conn, &id),
            Err(RequestRemovalError::Conflict)
        ));

        let mut conn_mut = pool.get().unwrap();
        retire(&mut conn_mut, &id, &kh).unwrap();
        let t = get(&conn, &id).unwrap().unwrap();
        assert!(t.retired_at.is_some());
        assert!(t.retirement_requested_at.is_none());
    }

    #[test]
    fn decline_removal_clears_the_request_without_retiring_and_is_audited() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let id = create(&conn, new_toy(&sub, &sub, "rope")).unwrap();
        request_removal(&conn, &id).unwrap();

        let mut conn_mut = pool.get().unwrap();
        decline_removal(&mut conn_mut, &id, &kh).unwrap();

        let t = get(&conn, &id).unwrap().unwrap();
        assert!(t.retirement_requested_at.is_none());
        assert!(t.retired_at.is_none());

        let audit_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM audit_log WHERE entity_id = ?1 AND action = 'toy.removal_declined'",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audit_count, 1);

        // Nothing pending anymore, so declining again is rejected.
        assert!(matches!(
            decline_removal(&mut conn_mut, &id, &kh),
            Err(DeclineRemovalError::NotPending)
        ));
    }
}
