//! Reward/punishment/task assignments (03-api-design.md §7).

use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, BlobDir, Pool};
use crate::domain::links;
use crate::domain::rewards_punishments::assignments::{
    self, Assignment, CreateError, EditError, ResolveError,
};

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const BAD_REQUEST: ApiError =
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "malformed request");
const CONFLICT: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "conflict",
    "request conflicts with current state",
);
const UNPROCESSABLE: ApiError = ApiError::new(
    StatusCode::UNPROCESSABLE_ENTITY,
    "unprocessable",
    "required fields are missing for this kind/effect_kind combination",
);
const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

#[derive(Serialize)]
pub struct AssignmentResponse {
    id: String,
    link_id: String,
    template_id: Option<String>,
    kind: String,
    title: String,
    description: Option<String>,
    effect_kind: Option<String>,
    completion_type: Option<String>,
    proof_media_types: Option<serde_json::Value>,
    proof_submission_id: Option<String>,
    deadline_at: Option<String>,
    time_extension_seconds: Option<i64>,
    time_reduction_seconds: Option<i64>,
    on_success_template_id: Option<String>,
    on_failure_template_id: Option<String>,
    escalated_from_assignment_id: Option<String>,
    triggered_by_submission_id: Option<String>,
    /// Not `points_balance`-crediting yet — Phase 6 owns the actual
    /// ledger (11-tasks-and-rewards.md §3). Surfaced here only as the
    /// value copied from the template at assignment time.
    points_delta: Option<i64>,
    assigned_at: String,
    assigned_by_user_id: Option<String>,
    assigned_via: String,
    status: String,
    notes: Option<String>,
}

impl From<Assignment> for AssignmentResponse {
    fn from(a: Assignment) -> Self {
        Self {
            id: a.id,
            link_id: a.link_id,
            template_id: a.template_id,
            kind: a.kind,
            title: a.title,
            description: a.description,
            effect_kind: a.effect_kind,
            completion_type: a.completion_type,
            proof_media_types: a
                .proof_media_types
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
            proof_submission_id: a.proof_submission_id,
            deadline_at: a.deadline_at.map(iso8601),
            time_extension_seconds: a.time_extension_seconds,
            time_reduction_seconds: a.time_reduction_seconds,
            on_success_template_id: a.on_success_template_id,
            on_failure_template_id: a.on_failure_template_id,
            escalated_from_assignment_id: a.escalated_from_assignment_id,
            triggered_by_submission_id: a.triggered_by_submission_id,
            points_delta: a.points_delta,
            assigned_at: iso8601(a.assigned_at),
            assigned_by_user_id: a.assigned_by_user_id,
            assigned_via: a.assigned_via,
            status: a.status,
            notes: a.notes,
        }
    }
}

fn map_create_error(e: CreateError) -> ApiError {
    match e {
        CreateError::TemplateNotFound => NOT_FOUND,
        CreateError::TemplateInactive => CONFLICT,
        CreateError::MissingKind
        | CreateError::MissingTitle
        | CreateError::MissingEffectKind
        | CreateError::MissingTimeExtensionSeconds
        | CreateError::MissingTimeReductionSeconds
        | CreateError::MissingCompletionType
        | CreateError::MissingProofMediaTypes => UNPROCESSABLE,
        CreateError::Db(_) => INTERNAL_ERROR,
    }
}

#[derive(Deserialize)]
pub struct CreateAssignmentRequest {
    kind: Option<String>,
    template_id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    effect_kind: Option<String>,
    time_extension_seconds: Option<i64>,
    time_reduction_seconds: Option<i64>,
    completion_type: Option<String>,
    proof_media_types: Option<serde_json::Value>,
    default_deadline_seconds: Option<i64>,
    deadline_at: Option<String>,
    on_success_template_id: Option<String>,
    on_failure_template_id: Option<String>,
    points_delta: Option<i64>,
    notes: Option<String>,
    triggered_by_submission_id: Option<String>,
}

fn parse_iso8601(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

async fn create_assignment(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<CreateAssignmentRequest>,
) -> Result<Json<AssignmentResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    let proof_media_types = req.proof_media_types.as_ref().map(|v| v.to_string());
    let deadline_at = req.deadline_at.as_deref().and_then(parse_iso8601);

    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;

        let a = assignments::create(
            &conn,
            &submissive_id,
            &link_id,
            assignments::NewAssignment {
                kind: req.kind.as_deref(),
                template_id: req.template_id.as_deref(),
                require_active_template: true,
                title: req.title.as_deref(),
                description: req.description.as_deref(),
                effect_kind: req.effect_kind.as_deref(),
                time_extension_seconds: req.time_extension_seconds,
                time_reduction_seconds: req.time_reduction_seconds,
                completion_type: req.completion_type.as_deref(),
                proof_media_types: proof_media_types.as_deref(),
                default_deadline_seconds: req.default_deadline_seconds,
                deadline_at,
                on_success_template_id: req.on_success_template_id.as_deref(),
                on_failure_template_id: req.on_failure_template_id.as_deref(),
                points_delta: req.points_delta,
                notes: req.notes.as_deref(),
                triggered_by_submission_id: req.triggered_by_submission_id.as_deref(),
                escalated_from_assignment_id: None,
                assigned_by_user_id: Some(&user.user_id),
                assigned_via: "session",
            },
        )
        .map_err(map_create_error)?;

        Ok(Json(a.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn list_for_submissive(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<Vec<AssignmentResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        let list = assignments::list_for_links(&conn, std::slice::from_ref(&link_id))
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn cross_roster_feed(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<AssignmentResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_ids = links::active_link_ids_for_keyholder(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?;
        let list = assignments::list_for_links(&conn, &link_ids).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

fn keyholder_owns_assignment(
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
        .map_err(|_| INTERNAL_ERROR)?;
    owns.map(|_| ()).ok_or(NOT_FOUND)
}

#[derive(Serialize)]
struct AssignmentDetail {
    #[serde(flatten)]
    assignment: AssignmentResponse,
    chain: Vec<AssignmentResponse>,
}

async fn assignment_detail(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(assignment_id): Path<String>,
) -> Result<Json<AssignmentDetail>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let a = assignments::get(&conn, &assignment_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        keyholder_owns_assignment(&conn, &user.user_id, &a.link_id)?;
        let chain = assignments::chain(&conn, &assignment_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(AssignmentDetail {
            assignment: a.into(),
            chain: chain.into_iter().map(Into::into).collect(),
        }))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn own_assignments(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<AssignmentResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let list = assignments::list_for_links(&conn, std::slice::from_ref(&link_id))
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn acknowledge(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(assignment_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        match assignments::acknowledge(&conn, &assignment_id, &user.user_id) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(assignments::AcknowledgeError::NotAcknowledgeable) => Err(CONFLICT),
            Err(assignments::AcknowledgeError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
pub struct ResolveRequest {
    status: String,
}

async fn resolve_assignment(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(assignment_id): Path<String>,
    Json(req): Json<ResolveRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    if !matches!(req.status.as_str(), "completed" | "revoked") {
        return Err(BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let mut conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        match assignments::resolve(&mut conn, &assignment_id, &user.user_id, &req.status) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(ResolveError::NotFound) => Err(NOT_FOUND),
            Err(ResolveError::InvalidTransition) => Err(CONFLICT),
            Err(ResolveError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
pub struct DeadlineRequest {
    deadline_at: String,
}

async fn edit_deadline(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(assignment_id): Path<String>,
    Json(req): Json<DeadlineRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    let Some(deadline_at) = parse_iso8601(&req.deadline_at) else {
        return Err(BAD_REQUEST);
    };
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        match assignments::edit_deadline(&conn, &assignment_id, &user.user_id, deadline_at) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(EditError::NotFound) => Err(NOT_FOUND),
            Err(EditError::AlreadyResolved) => Err(CONFLICT),
            Err(EditError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
pub struct EscalationRequest {
    on_failure_template_id: Option<String>,
}

async fn edit_escalation(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(assignment_id): Path<String>,
    Json(req): Json<EscalationRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        match assignments::edit_escalation(
            &conn,
            &assignment_id,
            &user.user_id,
            req.on_failure_template_id.as_deref(),
        ) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(EditError::NotFound) => Err(NOT_FOUND),
            Err(EditError::AlreadyResolved) => Err(CONFLICT),
            Err(EditError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn submit_task_proof(
    State(pool): State<Pool>,
    State(BlobDir(blob_dir)): State<BlobDir>,
    user: CurrentUser,
    Path(assignment_id): Path<String>,
    mut multipart: Multipart,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;

    let mut kind: Option<String> = None;
    let mut metadata: Option<String> = None;
    struct RawFile {
        content_type: String,
        bytes: Vec<u8>,
        filename: Option<String>,
    }
    let mut files: Vec<RawFile> = Vec::new();

    while let Some(field) = multipart.next_field().await.map_err(|_| BAD_REQUEST)? {
        match field.name().unwrap_or("") {
            "kind" => kind = Some(field.text().await.map_err(|_| BAD_REQUEST)?),
            "metadata" => metadata = Some(field.text().await.map_err(|_| BAD_REQUEST)?),
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

    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let mut conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let new_files: Vec<crate::domain::proofs::NewFile> = files
            .iter()
            .map(|f| crate::domain::proofs::NewFile {
                content_type: &f.content_type,
                bytes: &f.bytes,
                original_filename: f.filename.as_deref(),
            })
            .collect();
        match assignments::submit_proof(
            &mut conn,
            &blob_dir,
            &assignment_id,
            &user.user_id,
            &kind,
            metadata.as_deref(),
            new_files,
        ) {
            Ok(_) => Ok(StatusCode::OK),
            Err(assignments::SubmitProofError::NotFound) => Err(NOT_FOUND),
            Err(assignments::SubmitProofError::NotAwaitingProof) => Err(CONFLICT),
            Err(assignments::SubmitProofError::DeadlinePassed) => Err(CONFLICT),
            Err(assignments::SubmitProofError::Submit(_))
            | Err(assignments::SubmitProofError::Store(_))
            | Err(assignments::SubmitProofError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/keyholder/submissives/{id}/assignments",
            get(list_for_submissive).post(create_assignment),
        )
        .route("/keyholder/assignments", get(cross_roster_feed))
        .route(
            "/keyholder/assignments/{id}",
            get(assignment_detail).patch(resolve_assignment),
        )
        .route("/keyholder/assignments/{id}/deadline", patch(edit_deadline))
        .route(
            "/keyholder/assignments/{id}/escalation",
            patch(edit_escalation),
        )
        .route("/submissive/assignments", get(own_assignments))
        .route(
            "/submissive/assignments/{id}/acknowledge",
            patch(acknowledge),
        )
        .route(
            "/submissive/assignments/{id}/proof",
            post(submit_task_proof).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
}
