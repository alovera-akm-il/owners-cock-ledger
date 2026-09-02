//! Proof submissions: upload, review, and attachment streaming
//! (03-api-design.md §6, 04-verification-workflow.md §§3–4).

use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, BlobDir, Pool};
use crate::domain::proofs;
use crate::domain::{links, verification::policy};

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const BAD_REQUEST: ApiError =
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "malformed request");
const CONFLICT: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "conflict",
    "request conflicts with current state",
);
/// 25 MB per upload — generous for a handful of proof photos/a short
/// video, without leaving the endpoint unbounded.
const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

#[derive(Serialize)]
struct AttachmentSummary {
    id: String,
    mime_type: String,
    original_filename: Option<String>,
    byte_size: i64,
}

impl From<proofs::Attachment> for AttachmentSummary {
    fn from(a: proofs::Attachment) -> Self {
        Self {
            id: a.id,
            mime_type: a.mime_type,
            original_filename: a.original_filename,
            byte_size: a.byte_size,
        }
    }
}

#[derive(Serialize)]
struct SubmissionResponse {
    id: String,
    purpose: String,
    verification_code_value: Option<String>,
    kind: String,
    metadata: Option<String>,
    submitted_at: String,
    status: String,
    reviewed_by_user_id: Option<String>,
    reviewed_at: Option<String>,
    review_notes: Option<String>,
    reviewed_via: Option<String>,
    redo_of_submission_id: Option<String>,
    attachments: Vec<AttachmentSummary>,
}

fn submission_response(
    s: proofs::Submission,
    attachments: Vec<proofs::Attachment>,
) -> SubmissionResponse {
    SubmissionResponse {
        id: s.id,
        purpose: s.purpose,
        verification_code_value: s.verification_code_value,
        kind: s.kind,
        metadata: s.metadata,
        submitted_at: iso8601(s.submitted_at),
        status: s.status,
        reviewed_by_user_id: s.reviewed_by_user_id,
        reviewed_at: s.reviewed_at.map(iso8601),
        review_notes: s.review_notes,
        reviewed_via: s.reviewed_via,
        redo_of_submission_id: s.redo_of_submission_id,
        attachments: attachments.into_iter().map(Into::into).collect(),
    }
}

struct RawFile {
    content_type: String,
    bytes: Vec<u8>,
    filename: Option<String>,
}

async fn submit_proof(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<Json<SubmissionResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;

    let mut verification_code_id: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut metadata: Option<String> = None;
    let mut redo_of_submission_id: Option<String> = None;
    let mut files: Vec<RawFile> = Vec::new();

    while let Some(field) = multipart.next_field().await.map_err(|_| BAD_REQUEST)? {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "verification_code_id" => {
                let v = field.text().await.map_err(|_| BAD_REQUEST)?;
                verification_code_id = (!v.is_empty()).then_some(v);
            }
            "kind" => kind = Some(field.text().await.map_err(|_| BAD_REQUEST)?),
            "metadata" => metadata = Some(field.text().await.map_err(|_| BAD_REQUEST)?),
            "redo_of_submission_id" => {
                let v = field.text().await.map_err(|_| BAD_REQUEST)?;
                redo_of_submission_id = (!v.is_empty()).then_some(v);
            }
            "files" | "files[]" => {
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let filename = field.file_name().map(str::to_string);
                let bytes = field.bytes().await.map_err(|_| BAD_REQUEST)?.to_vec();
                files.push(RawFile {
                    content_type,
                    bytes,
                    filename,
                });
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let kind = kind.ok_or(BAD_REQUEST)?;

    let result = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let mut conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let grace_period_seconds = policy::get_for_link(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .map(|p| p.grace_period_seconds)
            .unwrap_or(0);

        let new_files: Vec<proofs::NewFile> = files
            .iter()
            .map(|f| proofs::NewFile {
                content_type: &f.content_type,
                bytes: &f.bytes,
                original_filename: f.filename.as_deref(),
            })
            .collect();

        let submission_id = proofs::submit(
            &mut conn,
            &blob_dir,
            proofs::NewSubmission {
                submissive_id: &user.user_id,
                link_id: &link_id,
                verification_code_id: verification_code_id.as_deref(),
                grace_period_seconds,
                kind: &kind,
                metadata: metadata.as_deref(),
                redo_of_submission_id: redo_of_submission_id.as_deref(),
                files: new_files,
            },
        )
        .map_err(|e| match e {
            proofs::SubmitError::InvalidOrExpiredCode => CONFLICT,
            proofs::SubmitError::InvalidRedoTarget => BAD_REQUEST,
            proofs::SubmitError::UnsupportedFile(_) => BAD_REQUEST,
            proofs::SubmitError::Db(_) | proofs::SubmitError::Io(_) => INTERNAL_ERROR,
        })?;

        let submission = proofs::get(&conn, &submission_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        let attachments =
            proofs::list_attachments(&conn, &submission_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(submission_response(submission, attachments))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    Ok(Json(result))
}

async fn own_submissions(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<SubmissionResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list = proofs::list_for_submissive(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        let mut out = Vec::with_capacity(list.len());
        for s in list {
            let attachments = proofs::list_attachments(&conn, &s.id).map_err(|_| INTERNAL_ERROR)?;
            out.push(submission_response(s, attachments));
        }
        Ok(Json(out))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn own_submission_detail(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submission_id): Path<String>,
) -> Result<Json<SubmissionResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let submission = proofs::get(&conn, &submission_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        if submission.submissive_id != user.user_id {
            return Err(NOT_FOUND);
        }
        let attachments =
            proofs::list_attachments(&conn, &submission_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(submission_response(submission, attachments)))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn submissions_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<Vec<SubmissionResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:proof-submissions")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        let list = proofs::list_for_links(&conn, &[link_id]).map_err(|_| INTERNAL_ERROR)?;
        let mut out = Vec::with_capacity(list.len());
        for s in list {
            let attachments = proofs::list_attachments(&conn, &s.id).map_err(|_| INTERNAL_ERROR)?;
            out.push(submission_response(s, attachments));
        }
        Ok(Json(out))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// Cross-roster feed (03-api-design.md §6) — every submission across the
/// caller's active links, newest first.
async fn cross_roster_feed(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<SubmissionResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:proof-submissions")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_ids = links::active_link_ids_for_keyholder(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?;
        let list = proofs::list_for_links(&conn, &link_ids).map_err(|_| INTERNAL_ERROR)?;
        let mut out = Vec::with_capacity(list.len());
        for s in list {
            let attachments = proofs::list_attachments(&conn, &s.id).map_err(|_| INTERNAL_ERROR)?;
            out.push(submission_response(s, attachments));
        }
        Ok(Json(out))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

fn keyholder_owns_submission(
    conn: &rusqlite::Connection,
    keyholder_id: &str,
    link_id: &str,
) -> Result<(), ApiError> {
    let owns: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM keyholder_submissive_links WHERE id = ?1 AND keyholder_id = ?2",
            rusqlite::params![link_id, keyholder_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| INTERNAL_ERROR)?;
    owns.map(|_| ()).ok_or(NOT_FOUND)
}

async fn submission_detail_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submission_id): Path<String>,
) -> Result<Json<SubmissionResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:proof-submissions")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let submission = proofs::get(&conn, &submission_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        keyholder_owns_submission(&conn, &user.user_id, &submission.link_id)?;
        let attachments =
            proofs::list_attachments(&conn, &submission_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(submission_response(submission, attachments)))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct ReviewRequest {
    status: String,
    review_notes: Option<String>,
}

async fn review_submission(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submission_id): Path<String>,
    Json(req): Json<ReviewRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("review:proof-submissions")
        .map_err(|_| FORBIDDEN)?;
    if !matches!(req.status.as_str(), "verified" | "redo" | "failed") {
        return Err(BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let mut conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let submission = proofs::get(&conn, &submission_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        keyholder_owns_submission(&conn, &user.user_id, &submission.link_id)?;

        // A review made by an API-token-driven script is worth being
        // able to tell apart from one a Keyholder actually looked at
        // (01-data-model.md §5, 05-security-and-privacy.md §9).
        let reviewed_via = if user.session_id().is_some() {
            "session"
        } else {
            "api_token"
        };

        // The same endpoint serves both ordinary verification and
        // task-completion proof (04-verification-workflow.md §7) —
        // review_proof does the submission update and, only when
        // purpose='punishment_completion', the linked assignment's
        // completed/failed transition and escalation, atomically.
        match crate::domain::rewards_punishments::assignments::review_proof(
            &mut conn,
            &submission_id,
            &submission.link_id,
            &req.status,
            req.review_notes.as_deref(),
            &user.user_id,
            reviewed_via,
        ) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(crate::domain::rewards_punishments::assignments::ReviewProofError::Proof(
                proofs::ReviewError::NotReviewable,
            )) => Err(CONFLICT),
            Err(_) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn download_attachment_keyholder(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    user: CurrentUser,
    Path((submission_id, attachment_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:proof-attachments")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<Response, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let submission = proofs::get(&conn, &submission_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        keyholder_owns_submission(&conn, &user.user_id, &submission.link_id)?;
        let attachment = proofs::get_attachment(&conn, &attachment_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let bytes = crate::storage::read(&blob_dir, &attachment.storage_path)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(([(header::CONTENT_TYPE, attachment.mime_type)], bytes).into_response())
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn download_attachment_submissive(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    user: CurrentUser,
    Path((submission_id, attachment_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<Response, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let submission = proofs::get(&conn, &submission_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        if submission.submissive_id != user.user_id {
            return Err(NOT_FOUND);
        }
        let attachment = proofs::get_attachment(&conn, &attachment_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let bytes = crate::storage::read(&blob_dir, &attachment.storage_path)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(([(header::CONTENT_TYPE, attachment.mime_type)], bytes).into_response())
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/submissive/proof-submissions",
            post(submit_proof).get(own_submissions),
        )
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
        .route(
            "/submissive/proof-submissions/{id}",
            get(own_submission_detail),
        )
        .route(
            "/submissive/proof-submissions/{id}/attachments/{attachmentId}",
            get(download_attachment_submissive),
        )
        .route(
            "/keyholder/submissives/{id}/proof-submissions",
            get(submissions_for_keyholder),
        )
        .route("/keyholder/proof-submissions", get(cross_roster_feed))
        .route(
            "/keyholder/proof-submissions/{id}",
            get(submission_detail_for_keyholder),
        )
        .route(
            "/keyholder/proof-submissions/{id}/attachments/{attachmentId}",
            get(download_attachment_keyholder),
        )
        .route(
            "/keyholder/proof-submissions/{id}/review",
            post(review_submission),
        )
}
