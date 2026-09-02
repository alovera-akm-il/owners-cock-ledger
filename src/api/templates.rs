//! Reward/punishment/task catalog (03-api-design.md §7).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, Pool};
use crate::domain::rewards_punishments::templates::{self, CreateError, Template, UpdateError};

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "template not found");
const UNPROCESSABLE: ApiError = ApiError::new(
    StatusCode::UNPROCESSABLE_ENTITY,
    "unprocessable",
    "required fields are missing for this kind/effect_kind combination",
);

#[derive(Serialize)]
pub struct TemplateResponse {
    id: String,
    kind: String,
    title: String,
    description: Option<String>,
    severity: Option<i64>,
    active: bool,
    effect_kind: Option<String>,
    completion_type: Option<String>,
    proof_media_types: Option<serde_json::Value>,
    default_deadline_seconds: Option<i64>,
    time_extension_seconds: Option<i64>,
    time_reduction_seconds: Option<i64>,
    on_success_template_id: Option<String>,
    on_failure_template_id: Option<String>,
    points_delta: Option<i64>,
    points_cost: Option<i64>,
}

impl From<Template> for TemplateResponse {
    fn from(t: Template) -> Self {
        Self {
            id: t.id,
            kind: t.kind,
            title: t.title,
            description: t.description,
            severity: t.severity,
            active: t.active,
            effect_kind: t.effect_kind,
            completion_type: t.completion_type,
            proof_media_types: t
                .proof_media_types
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
            default_deadline_seconds: t.default_deadline_seconds,
            time_extension_seconds: t.time_extension_seconds,
            time_reduction_seconds: t.time_reduction_seconds,
            on_success_template_id: t.on_success_template_id,
            on_failure_template_id: t.on_failure_template_id,
            points_delta: t.points_delta,
            points_cost: t.points_cost,
        }
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    kind: Option<String>,
}

async fn list_templates(
    State(pool): State<Pool>,
    user: CurrentUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<TemplateResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list = templates::list_for_keyholder(&conn, &user.user_id, query.kind.as_deref())
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
pub struct CreateTemplateRequest {
    kind: String,
    title: String,
    description: Option<String>,
    severity: Option<i64>,
    effect_kind: Option<String>,
    completion_type: Option<String>,
    proof_media_types: Option<serde_json::Value>,
    default_deadline_seconds: Option<i64>,
    time_extension_seconds: Option<i64>,
    time_reduction_seconds: Option<i64>,
    on_success_template_id: Option<String>,
    on_failure_template_id: Option<String>,
    points_delta: Option<i64>,
    points_cost: Option<i64>,
}

fn map_create_error(e: CreateError) -> ApiError {
    match e {
        CreateError::Validation(_) => UNPROCESSABLE,
        CreateError::Db(_) => INTERNAL_ERROR,
    }
}

async fn create_template(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<CreateTemplateRequest>,
) -> Result<Json<TemplateResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    let proof_media_types = req.proof_media_types.as_ref().map(|v| v.to_string());
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let id = templates::create(
            &conn,
            &user.user_id,
            templates::NewTemplate {
                kind: &req.kind,
                title: &req.title,
                description: req.description.as_deref(),
                severity: req.severity,
                effect_kind: req.effect_kind.as_deref(),
                completion_type: req.completion_type.as_deref(),
                proof_media_types: proof_media_types.as_deref(),
                default_deadline_seconds: req.default_deadline_seconds,
                time_extension_seconds: req.time_extension_seconds,
                time_reduction_seconds: req.time_reduction_seconds,
                on_success_template_id: req.on_success_template_id.as_deref(),
                on_failure_template_id: req.on_failure_template_id.as_deref(),
                points_delta: req.points_delta,
                points_cost: req.points_cost,
            },
        )
        .map_err(map_create_error)?;
        let t = templates::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        Ok(Json(t.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
pub struct PatchTemplateRequest {
    active: Option<bool>,
}

async fn patch_template(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<PatchTemplateRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        if req.active == Some(false) {
            match templates::deactivate(&conn, &id, &user.user_id) {
                Ok(()) => Ok(StatusCode::NO_CONTENT),
                Err(UpdateError::NotFound) => Err(NOT_FOUND),
                Err(UpdateError::Db(_)) => Err(INTERNAL_ERROR),
            }
        } else {
            // Full field editing isn't built yet — deactivation is the
            // one mutation the domain layer currently exposes.
            Ok(StatusCode::NO_CONTENT)
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/keyholder/templates",
            get(list_templates).post(create_template),
        )
        .route("/keyholder/templates/{id}", patch(patch_template))
}
