//! `proof_submissions`/`proof_attachments` (01-data-model.md §5,
//! 04-verification-workflow.md §§3–4): submit → review →
//! (verified | redo | failed).

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::auth::session::now;
use crate::domain::verification::codes;
use crate::storage;

pub struct Submission {
    pub id: String,
    pub submissive_id: String,
    pub link_id: String,
    pub purpose: String,
    pub verification_code_value: Option<String>,
    /// Set when `purpose='punishment_completion'` — which task/punishment
    /// assignment this proves completion of (01-data-model.md §5). Used
    /// by `rewards_punishments::assignments::review_proof` to find the
    /// linked assignment; `None` for an ordinary verification submission.
    pub assignment_id: Option<String>,
    pub kind: String,
    pub metadata: Option<String>,
    pub submitted_at: i64,
    pub status: String,
    pub reviewed_by_user_id: Option<String>,
    pub reviewed_at: Option<i64>,
    pub review_notes: Option<String>,
    pub reviewed_via: Option<String>,
    pub redo_of_submission_id: Option<String>,
}

const SUBMISSION_COLUMNS: &str = "id, submissive_id, link_id, purpose, verification_code_value, \
     assignment_id, kind, metadata, submitted_at, status, reviewed_by_user_id, reviewed_at, \
     review_notes, reviewed_via, redo_of_submission_id";

fn row_to_submission(row: &rusqlite::Row) -> rusqlite::Result<Submission> {
    Ok(Submission {
        id: row.get(0)?,
        submissive_id: row.get(1)?,
        link_id: row.get(2)?,
        purpose: row.get(3)?,
        verification_code_value: row.get(4)?,
        assignment_id: row.get(5)?,
        kind: row.get(6)?,
        metadata: row.get(7)?,
        submitted_at: row.get(8)?,
        status: row.get(9)?,
        reviewed_by_user_id: row.get(10)?,
        reviewed_at: row.get(11)?,
        review_notes: row.get(12)?,
        reviewed_via: row.get(13)?,
        redo_of_submission_id: row.get(14)?,
    })
}

pub struct Attachment {
    pub id: String,
    pub storage_path: String,
    pub original_filename: Option<String>,
    pub mime_type: String,
    pub byte_size: i64,
}

pub struct NewFile<'a> {
    pub content_type: &'a str,
    pub bytes: &'a [u8],
    pub original_filename: Option<&'a str>,
}

pub struct NewSubmission<'a> {
    pub submissive_id: &'a str,
    pub link_id: &'a str,
    /// `Some` redeems that code (validated against `link_id` and the
    /// policy's grace period); `None` is an unscheduled note, not gated
    /// by a compliance window.
    pub verification_code_id: Option<&'a str>,
    pub grace_period_seconds: i64,
    pub kind: &'a str,
    pub metadata: Option<&'a str>,
    pub redo_of_submission_id: Option<&'a str>,
    pub files: Vec<NewFile<'a>>,
}

#[derive(Debug, Error)]
pub enum SubmitError {
    #[error("verification code is missing, expired, consumed, or not yours")]
    InvalidOrExpiredCode,
    #[error("the submission being redone doesn't belong to you or isn't awaiting a redo")]
    InvalidRedoTarget,
    #[error("unsupported or invalid file: {0}")]
    UnsupportedFile(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<storage::StoreError> for SubmitError {
    fn from(e: storage::StoreError) -> Self {
        match e {
            storage::StoreError::UnsupportedContentType(ct) => SubmitError::UnsupportedFile(ct),
            storage::StoreError::InvalidImage => {
                SubmitError::UnsupportedFile("file is not a valid image".to_string())
            }
            storage::StoreError::Io(e) => SubmitError::Io(e),
        }
    }
}

/// `POST /submissive/proof-submissions` (04-verification-workflow.md
/// §3). Files are written to the blob directory before the DB
/// transaction opens — streaming disk IO shouldn't hold a SQLite
/// transaction open any longer than necessary.
pub fn submit(
    conn: &mut Connection,
    blob_dir: &Path,
    new: NewSubmission,
) -> Result<String, SubmitError> {
    let code = match new.verification_code_id {
        Some(code_id) => Some(
            codes::load_for_redemption(conn, code_id, new.link_id, new.grace_period_seconds)?
                .ok_or(SubmitError::InvalidOrExpiredCode)?,
        ),
        None => None,
    };

    if let Some(redo_id) = new.redo_of_submission_id {
        let valid: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM proof_submissions WHERE id = ?1 AND submissive_id = ?2 AND status = 'redo'",
                params![redo_id, new.submissive_id],
                |row| row.get(0),
            )
            .optional()?;
        if valid.is_none() {
            return Err(SubmitError::InvalidRedoTarget);
        }
    }

    let mut stored = Vec::with_capacity(new.files.len());
    for file in &new.files {
        stored.push((
            storage::store(blob_dir, file.content_type, file.bytes)?,
            file,
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let submitted_at = now();
    let tx = conn.transaction()?;

    tx.execute(
        "INSERT INTO proof_submissions
            (id, submissive_id, link_id, purpose, verification_code_id, verification_code_value,
             kind, metadata, submitted_at, redo_of_submission_id)
         VALUES (?1, ?2, ?3, 'verification', ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            new.submissive_id,
            new.link_id,
            new.verification_code_id,
            code.as_ref().map(|c| &c.code),
            new.kind,
            new.metadata,
            submitted_at,
            new.redo_of_submission_id,
        ],
    )?;

    for (stored_file, file) in &stored {
        tx.execute(
            "INSERT INTO proof_attachments
                (id, submission_id, storage_path, original_filename, mime_type, byte_size, sha256, uploaded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                uuid::Uuid::new_v4().to_string(),
                id,
                stored_file.storage_path,
                file.original_filename,
                file.content_type,
                stored_file.byte_size,
                stored_file.sha256,
                submitted_at,
            ],
        )?;
    }

    if let Some(code) = &code {
        codes::consume(&tx, &code.id, &id)?;
    }

    tx.commit()?;
    Ok(id)
}

pub fn get(conn: &Connection, submission_id: &str) -> rusqlite::Result<Option<Submission>> {
    conn.query_row(
        &format!("SELECT {SUBMISSION_COLUMNS} FROM proof_submissions WHERE id = ?1"),
        params![submission_id],
        row_to_submission,
    )
    .optional()
}

pub fn list_attachments(
    conn: &Connection,
    submission_id: &str,
) -> rusqlite::Result<Vec<Attachment>> {
    let mut stmt = conn.prepare(
        "SELECT id, storage_path, original_filename, mime_type, byte_size
         FROM proof_attachments WHERE submission_id = ?1 ORDER BY uploaded_at ASC",
    )?;
    stmt.query_map(params![submission_id], |row| {
        Ok(Attachment {
            id: row.get(0)?,
            storage_path: row.get(1)?,
            original_filename: row.get(2)?,
            mime_type: row.get(3)?,
            byte_size: row.get(4)?,
        })
    })?
    .collect()
}

pub fn get_attachment(
    conn: &Connection,
    attachment_id: &str,
) -> rusqlite::Result<Option<Attachment>> {
    conn.query_row(
        "SELECT id, storage_path, original_filename, mime_type, byte_size
         FROM proof_attachments WHERE id = ?1",
        params![attachment_id],
        |row| {
            Ok(Attachment {
                id: row.get(0)?,
                storage_path: row.get(1)?,
                original_filename: row.get(2)?,
                mime_type: row.get(3)?,
                byte_size: row.get(4)?,
            })
        },
    )
    .optional()
}

pub fn list_for_submissive(
    conn: &Connection,
    submissive_id: &str,
) -> rusqlite::Result<Vec<Submission>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SUBMISSION_COLUMNS} FROM proof_submissions
         WHERE submissive_id = ?1 ORDER BY submitted_at DESC"
    ))?;
    stmt.query_map(params![submissive_id], row_to_submission)?
        .collect()
}

/// Cross-roster feed (`GET /keyholder/proof-submissions`,
/// 03-api-design.md §6) — every submission across the given links,
/// newest first.
pub fn list_for_links(conn: &Connection, link_ids: &[String]) -> rusqlite::Result<Vec<Submission>> {
    if link_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = link_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT {SUBMISSION_COLUMNS} FROM proof_submissions
         WHERE link_id IN ({placeholders}) ORDER BY submitted_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = link_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();
    stmt.query_map(params.as_slice(), row_to_submission)?
        .collect()
}

#[derive(Debug, Error)]
pub enum ReviewError {
    #[error("submission not found, not yours to review, or already reviewed")]
    NotReviewable,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST /keyholder/proof-submissions/{id}/review`
/// (04-verification-workflow.md §4). Attaching a punishment on `failed`
/// isn't supported yet — that needs the `assignments` table, Phase 3.
pub fn review(
    conn: &Connection,
    submission_id: &str,
    link_id: &str,
    status: &str,
    review_notes: Option<&str>,
    reviewed_by_user_id: &str,
    reviewed_via: &str,
) -> Result<(), ReviewError> {
    let affected = conn.execute(
        "UPDATE proof_submissions SET
            status = ?1, reviewed_by_user_id = ?2, reviewed_at = ?3,
            review_notes = ?4, reviewed_via = ?5
         WHERE id = ?6 AND link_id = ?7 AND status = 'pending'",
        params![
            status,
            reviewed_by_user_id,
            now(),
            review_notes,
            reviewed_via,
            submission_id,
            link_id,
        ],
    )?;
    if affected == 0 {
        return Err(ReviewError::NotReviewable);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::verification::policy;

    fn temp_setup() -> (
        tempfile::TempDir,
        crate::db::Pool,
        tempfile::TempDir,
        String,
        String,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
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
        (dir, pool, blob_dir, submissive_id, link_id)
    }

    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn submit_with_code_consumes_it_and_snapshots_the_value() {
        let (_dir, pool, blob_dir, submissive_id, link_id) = temp_setup();
        let mut conn = pool.get().unwrap();
        let policy = policy::get_for_link(&conn, &link_id).unwrap().unwrap();
        let code = codes::request_on_demand(&conn, &policy).unwrap();

        let submission_id = submit(
            &mut conn,
            blob_dir.path(),
            NewSubmission {
                submissive_id: &submissive_id,
                link_id: &link_id,
                verification_code_id: Some(&code.id),
                grace_period_seconds: policy.grace_period_seconds,
                kind: "photo",
                metadata: None,
                redo_of_submission_id: None,
                files: vec![NewFile {
                    content_type: "image/png",
                    bytes: TINY_PNG,
                    original_filename: Some("proof.png"),
                }],
            },
        )
        .unwrap();

        let submission = get(&conn, &submission_id).unwrap().unwrap();
        assert_eq!(
            submission.verification_code_value.as_deref(),
            Some(code.code.as_str())
        );
        assert_eq!(submission.status, "pending");

        assert!(
            codes::current_unconsumed(&conn, &link_id)
                .unwrap()
                .is_none()
        );

        let attachments = list_attachments(&conn, &submission_id).unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].mime_type, "image/png");
    }

    #[test]
    fn submit_without_code_is_an_unscheduled_note() {
        let (_dir, pool, blob_dir, submissive_id, link_id) = temp_setup();
        let mut conn = pool.get().unwrap();

        let submission_id = submit(
            &mut conn,
            blob_dir.path(),
            NewSubmission {
                submissive_id: &submissive_id,
                link_id: &link_id,
                verification_code_id: None,
                grace_period_seconds: 600,
                kind: "note",
                metadata: Some(r#"{"mood":"fine"}"#),
                redo_of_submission_id: None,
                files: vec![],
            },
        )
        .unwrap();

        let submission = get(&conn, &submission_id).unwrap().unwrap();
        assert!(submission.verification_code_value.is_none());
    }

    #[test]
    fn submit_with_invalid_code_is_rejected() {
        let (_dir, pool, blob_dir, submissive_id, link_id) = temp_setup();
        let mut conn = pool.get().unwrap();

        let result = submit(
            &mut conn,
            blob_dir.path(),
            NewSubmission {
                submissive_id: &submissive_id,
                link_id: &link_id,
                verification_code_id: Some("not-a-real-code"),
                grace_period_seconds: 600,
                kind: "note",
                metadata: None,
                redo_of_submission_id: None,
                files: vec![],
            },
        );
        assert!(matches!(result, Err(SubmitError::InvalidOrExpiredCode)));
    }

    #[test]
    fn review_then_second_review_is_rejected() {
        let (_dir, pool, blob_dir, submissive_id, link_id) = temp_setup();
        let mut conn = pool.get().unwrap();
        let keyholder_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH2', 0)",
            params![keyholder_id],
        )
        .unwrap();

        let submission_id = submit(
            &mut conn,
            blob_dir.path(),
            NewSubmission {
                submissive_id: &submissive_id,
                link_id: &link_id,
                verification_code_id: None,
                grace_period_seconds: 600,
                kind: "note",
                metadata: None,
                redo_of_submission_id: None,
                files: vec![],
            },
        )
        .unwrap();

        review(
            &conn,
            &submission_id,
            &link_id,
            "verified",
            None,
            &keyholder_id,
            "session",
        )
        .unwrap();
        let second = review(
            &conn,
            &submission_id,
            &link_id,
            "verified",
            None,
            &keyholder_id,
            "session",
        );
        assert!(matches!(second, Err(ReviewError::NotReviewable)));

        let submission = get(&conn, &submission_id).unwrap().unwrap();
        assert_eq!(submission.status, "verified");
        assert_eq!(submission.reviewed_via.as_deref(), Some("session"));
    }

    #[test]
    fn redo_chains_to_a_submission_in_redo_status() {
        let (_dir, pool, blob_dir, submissive_id, link_id) = temp_setup();
        let mut conn = pool.get().unwrap();
        let keyholder_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH3', 0)",
            params![keyholder_id],
        )
        .unwrap();

        let first_id = submit(
            &mut conn,
            blob_dir.path(),
            NewSubmission {
                submissive_id: &submissive_id,
                link_id: &link_id,
                verification_code_id: None,
                grace_period_seconds: 600,
                kind: "note",
                metadata: None,
                redo_of_submission_id: None,
                files: vec![],
            },
        )
        .unwrap();
        review(
            &conn,
            &first_id,
            &link_id,
            "redo",
            Some("try again"),
            &keyholder_id,
            "session",
        )
        .unwrap();

        let redo_id = submit(
            &mut conn,
            blob_dir.path(),
            NewSubmission {
                submissive_id: &submissive_id,
                link_id: &link_id,
                verification_code_id: None,
                grace_period_seconds: 600,
                kind: "note",
                metadata: None,
                redo_of_submission_id: Some(&first_id),
                files: vec![],
            },
        )
        .unwrap();

        let redo = get(&conn, &redo_id).unwrap().unwrap();
        assert_eq!(
            redo.redo_of_submission_id.as_deref(),
            Some(first_id.as_str())
        );
    }

    #[test]
    fn redo_against_a_non_redo_submission_is_rejected() {
        let (_dir, pool, blob_dir, submissive_id, link_id) = temp_setup();
        let mut conn = pool.get().unwrap();

        let first_id = submit(
            &mut conn,
            blob_dir.path(),
            NewSubmission {
                submissive_id: &submissive_id,
                link_id: &link_id,
                verification_code_id: None,
                grace_period_seconds: 600,
                kind: "note",
                metadata: None,
                redo_of_submission_id: None,
                files: vec![],
            },
        )
        .unwrap();

        let result = submit(
            &mut conn,
            blob_dir.path(),
            NewSubmission {
                submissive_id: &submissive_id,
                link_id: &link_id,
                verification_code_id: None,
                grace_period_seconds: 600,
                kind: "note",
                metadata: None,
                redo_of_submission_id: Some(&first_id),
                files: vec![],
            },
        );
        assert!(matches!(result, Err(SubmitError::InvalidRedoTarget)));
    }

    #[test]
    fn list_for_links_spans_multiple_links() {
        let (_dir, pool, blob_dir, submissive_id, link_id) = temp_setup();
        let mut conn = pool.get().unwrap();
        submit(
            &mut conn,
            blob_dir.path(),
            NewSubmission {
                submissive_id: &submissive_id,
                link_id: &link_id,
                verification_code_id: None,
                grace_period_seconds: 600,
                kind: "note",
                metadata: None,
                redo_of_submission_id: None,
                files: vec![],
            },
        )
        .unwrap();

        let results = list_for_links(&conn, std::slice::from_ref(&link_id)).unwrap();
        assert_eq!(results.len(), 1);

        let empty = list_for_links(&conn, &[]).unwrap();
        assert!(empty.is_empty());
    }
}
