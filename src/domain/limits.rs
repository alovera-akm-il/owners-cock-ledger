//! Structured hard/soft limits (06-future-extensions.md §9) — the
//! checkable layer on top of the free-text `hard_limits`/`soft_limits`
//! fields on `keyholder_profiles`/`submissive_profiles`
//! (`domain::profiles`), which stay exactly as they are and are not
//! touched by this module. `limit_items` is a reusable catalog
//! (global seed rows plus a Keyholder's own additions);
//! `submissive_limit_ratings` is one submissive's rating of one item.
//! No row at all is the default "not discussed" state — this module
//! never coerces a missing rating into "okay".

use rusqlite::{Connection, OptionalExtension, params};

pub struct LimitItem {
    pub id: String,
    /// `None` = a global seed item, ships with every deployment.
    /// `Some` = a Keyholder's own addition, visible only to their own
    /// submissives.
    pub keyholder_id: Option<String>,
    pub category: String,
    pub label: String,
    pub description: Option<String>,
    pub active: bool,
    pub created_at: i64,
}

const ITEM_COLUMNS: &str = "id, keyholder_id, category, label, description, active, created_at";

fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<LimitItem> {
    Ok(LimitItem {
        id: row.get(0)?,
        keyholder_id: row.get(1)?,
        category: row.get(2)?,
        label: row.get(3)?,
        description: row.get(4)?,
        active: row.get(5)?,
        created_at: row.get(6)?,
    })
}

pub fn get_item(conn: &Connection, id: &str) -> rusqlite::Result<Option<LimitItem>> {
    conn.query_row(
        &format!("SELECT {ITEM_COLUMNS} FROM limit_items WHERE id = ?1"),
        params![id],
        row_to_item,
    )
    .optional()
}

/// Global seed items plus this Keyholder's own — every caller filters
/// `active` for its own purposes (the Keyholder's own management view
/// wants to see inactive ones too, to be able to reactivate them).
pub fn list_items_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<LimitItem>> {
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM limit_items WHERE keyholder_id IS NULL OR keyholder_id = ?1
         ORDER BY category ASC, label ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![keyholder_id], row_to_item)?
        .collect()
}

pub struct NewItem<'a> {
    pub keyholder_id: &'a str,
    pub category: &'a str,
    pub label: &'a str,
    pub description: Option<&'a str>,
}

/// `POST /keyholder/limit-items` — a Keyholder's own addition to the
/// catalog; global seed items are never created through this path.
pub fn create_item(conn: &Connection, new: NewItem) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO limit_items (id, keyholder_id, category, label, description, active, created_at)
         VALUES (?1,?2,?3,?4,?5,1,?6)",
        params![
            id,
            new.keyholder_id,
            new.category,
            new.label,
            new.description,
            crate::auth::session::now(),
        ],
    )?;
    Ok(id)
}

#[derive(Default)]
pub struct ItemEdit<'a> {
    pub category: Option<&'a str>,
    pub label: Option<&'a str>,
    pub description: Option<Option<&'a str>>,
    pub active: Option<bool>,
}

/// `PATCH /keyholder/limit-items/{id}` — a Keyholder may only edit
/// their own additions, never a global seed item (`keyholder_id`
/// `NULL`) or another Keyholder's. Returns `false` on any of those,
/// same 404-not-403 posture as the rest of this API.
pub fn update_item(
    conn: &Connection,
    id: &str,
    keyholder_id: &str,
    edit: ItemEdit,
) -> rusqlite::Result<bool> {
    let Some(current) = get_item(conn, id)? else {
        return Ok(false);
    };
    if current.keyholder_id.as_deref() != Some(keyholder_id) {
        return Ok(false);
    }
    let category = edit.category.unwrap_or(&current.category);
    let label = edit.label.unwrap_or(&current.label);
    let description = edit.description.unwrap_or(current.description.as_deref());
    let active = edit.active.unwrap_or(current.active);

    conn.execute(
        "UPDATE limit_items SET category = ?1, label = ?2, description = ?3, active = ?4
         WHERE id = ?5",
        params![category, label, description, active, id],
    )?;
    Ok(true)
}

pub struct Rating {
    pub submissive_id: String,
    pub limit_item_id: String,
    pub rating: String,
    pub notes: Option<String>,
    pub updated_at: i64,
}

fn row_to_rating(row: &rusqlite::Row) -> rusqlite::Result<Rating> {
    Ok(Rating {
        submissive_id: row.get(0)?,
        limit_item_id: row.get(1)?,
        rating: row.get(2)?,
        notes: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

const RATING_COLUMNS: &str = "submissive_id, limit_item_id, rating, notes, updated_at";

pub fn list_ratings_for_submissive(
    conn: &Connection,
    submissive_id: &str,
) -> rusqlite::Result<Vec<Rating>> {
    let sql =
        format!("SELECT {RATING_COLUMNS} FROM submissive_limit_ratings WHERE submissive_id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![submissive_id], row_to_rating)?
        .collect()
}

/// One item paired with this submissive's rating of it, if any —
/// exactly what both the submissive's own rating view and the
/// Keyholder's read-only view of one submissive render (`None` means
/// "not discussed", surfaced as such rather than silently omitted).
pub fn list_items_with_ratings_for_submissive(
    conn: &Connection,
    keyholder_id: &str,
    submissive_id: &str,
) -> rusqlite::Result<Vec<(LimitItem, Option<Rating>)>> {
    let items = list_items_for_keyholder(conn, keyholder_id)?
        .into_iter()
        .filter(|i| i.active)
        .collect::<Vec<_>>();
    let ratings = list_ratings_for_submissive(conn, submissive_id)?;
    Ok(items
        .into_iter()
        .map(|item| {
            let rating = ratings
                .iter()
                .find(|r| r.limit_item_id == item.id)
                .map(|r| Rating {
                    submissive_id: r.submissive_id.clone(),
                    limit_item_id: r.limit_item_id.clone(),
                    rating: r.rating.clone(),
                    notes: r.notes.clone(),
                    updated_at: r.updated_at,
                });
            (item, rating)
        })
        .collect())
}

pub struct KeyholderRating {
    pub keyholder_id: String,
    pub limit_item_id: String,
    pub rating: String,
    pub notes: Option<String>,
    pub updated_at: i64,
}

fn row_to_keyholder_rating(row: &rusqlite::Row) -> rusqlite::Result<KeyholderRating> {
    Ok(KeyholderRating {
        keyholder_id: row.get(0)?,
        limit_item_id: row.get(1)?,
        rating: row.get(2)?,
        notes: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

const KEYHOLDER_RATING_COLUMNS: &str = "keyholder_id, limit_item_id, rating, notes, updated_at";

pub fn list_ratings_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<KeyholderRating>> {
    let sql = format!(
        "SELECT {KEYHOLDER_RATING_COLUMNS} FROM keyholder_limit_ratings WHERE keyholder_id = ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![keyholder_id], row_to_keyholder_rating)?
        .collect()
}

/// Same idea as `list_items_with_ratings_for_submissive`, but the
/// catalog-owner and the rating-owner are the same person here — a
/// Keyholder rates their own catalog, rather than a submissive rating
/// their Keyholder's.
pub fn list_items_with_ratings_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<(LimitItem, Option<KeyholderRating>)>> {
    let items = list_items_for_keyholder(conn, keyholder_id)?
        .into_iter()
        .filter(|i| i.active)
        .collect::<Vec<_>>();
    let ratings = list_ratings_for_keyholder(conn, keyholder_id)?;
    Ok(items
        .into_iter()
        .map(|item| {
            let rating = ratings
                .iter()
                .find(|r| r.limit_item_id == item.id)
                .map(|r| KeyholderRating {
                    keyholder_id: r.keyholder_id.clone(),
                    limit_item_id: r.limit_item_id.clone(),
                    rating: r.rating.clone(),
                    notes: r.notes.clone(),
                    updated_at: r.updated_at,
                });
            (item, rating)
        })
        .collect())
}

/// `PUT /keyholder/limit-ratings/{item_id}` — a Keyholder rating their
/// own catalog, mirroring `set_rating` exactly.
pub fn set_keyholder_rating(
    conn: &Connection,
    keyholder_id: &str,
    limit_item_id: &str,
    rating: &str,
    notes: Option<&str>,
) -> Result<(), SetRatingError> {
    if !matches!(rating, "hard" | "soft" | "okay") {
        return Err(SetRatingError::InvalidRating);
    }
    if get_item(conn, limit_item_id)?.is_none() {
        return Err(SetRatingError::ItemNotFound);
    }
    conn.execute(
        "INSERT INTO keyholder_limit_ratings (id, keyholder_id, limit_item_id, rating, notes, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT (keyholder_id, limit_item_id)
         DO UPDATE SET rating = excluded.rating, notes = excluded.notes, updated_at = excluded.updated_at",
        params![
            uuid::Uuid::new_v4().to_string(),
            keyholder_id,
            limit_item_id,
            rating,
            notes,
            crate::auth::session::now(),
        ],
    )?;
    Ok(())
}

/// `DELETE /keyholder/limit-ratings/{item_id}` — mirrors `clear_rating`.
pub fn clear_keyholder_rating(
    conn: &Connection,
    keyholder_id: &str,
    limit_item_id: &str,
) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "DELETE FROM keyholder_limit_ratings WHERE keyholder_id = ?1 AND limit_item_id = ?2",
        params![keyholder_id, limit_item_id],
    )?;
    Ok(affected > 0)
}

#[derive(Debug, thiserror::Error)]
pub enum SetRatingError {
    #[error("invalid rating value")]
    InvalidRating,
    #[error("no such limit item")]
    ItemNotFound,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PUT /submissive/limit-ratings/{item_id}` — a submissive rates a
/// catalog item as it applies to them; upserts on
/// `(submissive_id, limit_item_id)`. Rating a limit is exactly as
/// submissive-owned an act as writing the free-text paragraph already
/// is (06-future-extensions.md §9).
pub fn set_rating(
    conn: &Connection,
    submissive_id: &str,
    limit_item_id: &str,
    rating: &str,
    notes: Option<&str>,
) -> Result<(), SetRatingError> {
    if !matches!(rating, "hard" | "soft" | "okay") {
        return Err(SetRatingError::InvalidRating);
    }
    if get_item(conn, limit_item_id)?.is_none() {
        return Err(SetRatingError::ItemNotFound);
    }
    conn.execute(
        "INSERT INTO submissive_limit_ratings (id, submissive_id, limit_item_id, rating, notes, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT (submissive_id, limit_item_id)
         DO UPDATE SET rating = excluded.rating, notes = excluded.notes, updated_at = excluded.updated_at",
        params![
            uuid::Uuid::new_v4().to_string(),
            submissive_id,
            limit_item_id,
            rating,
            notes,
            crate::auth::session::now(),
        ],
    )?;
    Ok(())
}

/// `DELETE /submissive/limit-ratings/{item_id}` — back to "not
/// discussed", not just cleared to some default. Returns `false` if
/// there was nothing to clear.
pub fn clear_rating(
    conn: &Connection,
    submissive_id: &str,
    limit_item_id: &str,
) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "DELETE FROM submissive_limit_ratings WHERE submissive_id = ?1 AND limit_item_id = ?2",
        params![submissive_id, limit_item_id],
    )?;
    Ok(affected > 0)
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

    #[test]
    fn list_items_for_keyholder_includes_global_seed_rows() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, _sub) = seed_users(&conn);

        let items = list_items_for_keyholder(&conn, &kh).unwrap();
        assert!(!items.is_empty());
        assert!(items.iter().all(|i| i.keyholder_id.is_none()));
    }

    #[test]
    fn create_item_is_scoped_to_the_owning_keyholder_only() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, _sub) = seed_users(&conn);
        let other_kh = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH2', 0)",
            params![other_kh],
        )
        .unwrap();

        let id = create_item(
            &conn,
            NewItem {
                keyholder_id: &kh,
                category: "Custom",
                label: "House rule",
                description: None,
            },
        )
        .unwrap();

        assert!(
            list_items_for_keyholder(&conn, &kh)
                .unwrap()
                .iter()
                .any(|i| i.id == id)
        );
        assert!(
            !list_items_for_keyholder(&conn, &other_kh)
                .unwrap()
                .iter()
                .any(|i| i.id == id)
        );
    }

    #[test]
    fn update_item_rejects_editing_a_global_seed_item_or_someone_elses() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, _sub) = seed_users(&conn);

        assert!(!update_item(&conn, "seed-impact-paddle", &kh, ItemEdit::default()).unwrap());

        let own_id = create_item(
            &conn,
            NewItem {
                keyholder_id: &kh,
                category: "Custom",
                label: "House rule",
                description: None,
            },
        )
        .unwrap();
        let other_kh = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH2', 0)",
            params![other_kh],
        )
        .unwrap();
        assert!(!update_item(&conn, &own_id, &other_kh, ItemEdit::default()).unwrap());
        assert!(
            update_item(
                &conn,
                &own_id,
                &kh,
                ItemEdit {
                    label: Some("Renamed"),
                    ..Default::default()
                },
            )
            .unwrap()
        );
        assert_eq!(get_item(&conn, &own_id).unwrap().unwrap().label, "Renamed");
    }

    #[test]
    fn set_rating_upserts_and_rejects_an_invalid_value() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (_kh, sub) = seed_users(&conn);

        assert!(matches!(
            set_rating(&conn, &sub, "seed-impact-paddle", "bogus", None),
            Err(SetRatingError::InvalidRating)
        ));
        assert!(matches!(
            set_rating(&conn, &sub, "no-such-item", "hard", None),
            Err(SetRatingError::ItemNotFound)
        ));

        set_rating(&conn, &sub, "seed-impact-paddle", "hard", Some("no")).unwrap();
        let ratings = list_ratings_for_submissive(&conn, &sub).unwrap();
        assert_eq!(ratings.len(), 1);
        assert_eq!(ratings[0].rating, "hard");

        // Upsert: rating the same item again replaces, doesn't duplicate.
        set_rating(&conn, &sub, "seed-impact-paddle", "soft", None).unwrap();
        let ratings = list_ratings_for_submissive(&conn, &sub).unwrap();
        assert_eq!(ratings.len(), 1);
        assert_eq!(ratings[0].rating, "soft");
        assert!(ratings[0].notes.is_none());
    }

    #[test]
    fn clear_rating_returns_to_not_discussed() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (_kh, sub) = seed_users(&conn);

        assert!(!clear_rating(&conn, &sub, "seed-impact-paddle").unwrap());
        set_rating(&conn, &sub, "seed-impact-paddle", "hard", None).unwrap();
        assert!(clear_rating(&conn, &sub, "seed-impact-paddle").unwrap());
        assert!(list_ratings_for_submissive(&conn, &sub).unwrap().is_empty());
    }

    #[test]
    fn list_items_with_ratings_pairs_correctly_and_leaves_unrated_as_none() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);

        set_rating(&conn, &sub, "seed-impact-paddle", "hard", None).unwrap();
        let paired = list_items_with_ratings_for_submissive(&conn, &kh, &sub).unwrap();
        let paddle = paired
            .iter()
            .find(|(i, _)| i.id == "seed-impact-paddle")
            .unwrap();
        assert_eq!(paddle.1.as_ref().unwrap().rating, "hard");

        let cane = paired
            .iter()
            .find(|(i, _)| i.id == "seed-impact-cane")
            .unwrap();
        assert!(cane.1.is_none());
    }

    #[test]
    fn set_keyholder_rating_upserts_and_rejects_an_invalid_value() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, _sub) = seed_users(&conn);

        assert!(matches!(
            set_keyholder_rating(&conn, &kh, "seed-impact-paddle", "bogus", None),
            Err(SetRatingError::InvalidRating)
        ));
        assert!(matches!(
            set_keyholder_rating(&conn, &kh, "no-such-item", "hard", None),
            Err(SetRatingError::ItemNotFound)
        ));

        set_keyholder_rating(&conn, &kh, "seed-impact-paddle", "hard", Some("no")).unwrap();
        let ratings = list_ratings_for_keyholder(&conn, &kh).unwrap();
        assert_eq!(ratings.len(), 1);
        assert_eq!(ratings[0].rating, "hard");

        // Upsert: rating the same item again replaces, doesn't duplicate.
        set_keyholder_rating(&conn, &kh, "seed-impact-paddle", "soft", None).unwrap();
        let ratings = list_ratings_for_keyholder(&conn, &kh).unwrap();
        assert_eq!(ratings.len(), 1);
        assert_eq!(ratings[0].rating, "soft");
        assert!(ratings[0].notes.is_none());
    }

    #[test]
    fn clear_keyholder_rating_returns_to_not_discussed() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, _sub) = seed_users(&conn);

        assert!(!clear_keyholder_rating(&conn, &kh, "seed-impact-paddle").unwrap());
        set_keyholder_rating(&conn, &kh, "seed-impact-paddle", "hard", None).unwrap();
        assert!(clear_keyholder_rating(&conn, &kh, "seed-impact-paddle").unwrap());
        assert!(
            list_ratings_for_keyholder(&conn, &kh)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn list_items_with_keyholder_ratings_pairs_correctly_and_leaves_unrated_as_none() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, _sub) = seed_users(&conn);

        set_keyholder_rating(&conn, &kh, "seed-impact-paddle", "hard", None).unwrap();
        let paired = list_items_with_ratings_for_keyholder(&conn, &kh).unwrap();
        let paddle = paired
            .iter()
            .find(|(i, _)| i.id == "seed-impact-paddle")
            .unwrap();
        assert_eq!(paddle.1.as_ref().unwrap().rating, "hard");

        let cane = paired
            .iter()
            .find(|(i, _)| i.id == "seed-impact-cane")
            .unwrap();
        assert!(cane.1.is_none());
    }

    #[test]
    fn keyholder_ratings_are_scoped_to_the_rating_keyholder_only() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (kh, _sub) = seed_users(&conn);
        let other_kh = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH2', 0)",
            params![other_kh],
        )
        .unwrap();

        set_keyholder_rating(&conn, &kh, "seed-impact-paddle", "hard", None).unwrap();
        assert!(
            list_ratings_for_keyholder(&conn, &other_kh)
                .unwrap()
                .is_empty()
        );
    }
}
