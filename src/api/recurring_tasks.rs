//! Repeating tasks (06-future-extensions.md §14) — a rule that
//! periodically spawns an ordinary `assignments` row from an existing
//! `kind='task'` template. Keyholder-only, same authorship posture as
//! every other template/rule in this schema.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, Pool};
use crate::domain::{links, recurring_tasks};

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const BAD_REQUEST: ApiError =
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "malformed request");
const UNPROCESSABLE: ApiError = ApiError::new(
    StatusCode::UNPROCESSABLE_ENTITY,
    "unprocessable",
    "the template isn't usable for a recurring rule",
);

#[derive(Serialize)]
struct RuleResponse {
    id: String,
    template_id: String,
    recurrence_kind: String,
    recurrence_value: serde_json::Value,
    allow_overlap: bool,
    active: bool,
    next_due_at: String,
    created_at: String,
}

impl From<recurring_tasks::Rule> for RuleResponse {
    fn from(r: recurring_tasks::Rule) -> Self {
        Self {
            id: r.id,
            template_id: r.template_id,
            recurrence_kind: r.recurrence_kind,
            recurrence_value: serde_json::from_str(&r.recurrence_value)
                .unwrap_or(serde_json::Value::Null),
            allow_overlap: r.allow_overlap,
            active: r.active,
            next_due_at: iso8601(r.next_due_at),
            created_at: iso8601(r.created_at),
        }
    }
}

async fn list_rules(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<Vec<RuleResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        let list = recurring_tasks::list_for_link(&conn, &link_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct CreateRuleRequest {
    template_id: String,
    recurrence_kind: String,
    recurrence_value: serde_json::Value,
    #[serde(default)]
    allow_overlap: bool,
}

fn valid_recurrence_kind(kind: &str) -> bool {
    matches!(kind, "interval_hours" | "daily" | "weekly_days")
}

/// `POST /keyholder/submissives/{id}/recurring-tasks`.
async fn create_rule(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<CreateRuleRequest>,
) -> Result<Json<RuleResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:assignments")
        .map_err(|_| FORBIDDEN)?;
    if !valid_recurrence_kind(&req.recurrence_kind) {
        return Err(BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        let recurrence_value = req.recurrence_value.to_string();
        let id = recurring_tasks::create(
            &conn,
            recurring_tasks::NewRule {
                link_id: &link_id,
                keyholder_id: &user.user_id,
                template_id: &req.template_id,
                recurrence_kind: &req.recurrence_kind,
                recurrence_value: &recurrence_value,
                allow_overlap: req.allow_overlap,
            },
        )
        .map_err(|e| match e {
            recurring_tasks::RuleError::TemplateNotFound => NOT_FOUND,
            recurring_tasks::RuleError::NotATaskTemplate => UNPROCESSABLE,
            recurring_tasks::RuleError::InvalidRecurrence => BAD_REQUEST,
            recurring_tasks::RuleError::Db(_) => INTERNAL_ERROR,
        })?;
        let rule = recurring_tasks::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        Ok(Json(rule.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize, Default)]
struct PatchRuleRequest {
    recurrence_kind: Option<String>,
    recurrence_value: Option<serde_json::Value>,
    allow_overlap: Option<bool>,
    active: Option<bool>,
}

/// `PATCH /keyholder/recurring-tasks/{id}` — resolves ownership via
/// the rule's own `link_id` rather than a `{submissive_id}` path
/// segment, since a rule id alone is enough once we've confirmed it
/// belongs to one of this Keyholder's own links.
async fn patch_rule(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<PatchRuleRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:assignments")
        .map_err(|_| FORBIDDEN)?;
    if let Some(kind) = &req.recurrence_kind
        && !valid_recurrence_kind(kind)
    {
        return Err(BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let rule = recurring_tasks::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let (keyholder_id, _) = links::parties(&conn, &rule.link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        if keyholder_id != user.user_id {
            return Err(NOT_FOUND);
        }
        let recurrence_value = req.recurrence_value.as_ref().map(|v| v.to_string());
        let updated = recurring_tasks::update(
            &conn,
            &id,
            &rule.link_id,
            recurring_tasks::RuleEdit {
                recurrence_kind: req.recurrence_kind.as_deref(),
                recurrence_value: recurrence_value.as_deref(),
                allow_overlap: req.allow_overlap,
                active: req.active,
            },
        )
        .map_err(|e| match e {
            recurring_tasks::RuleError::InvalidRecurrence => BAD_REQUEST,
            _ => INTERNAL_ERROR,
        })?;
        if !updated {
            return Err(NOT_FOUND);
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/keyholder/submissives/{id}/recurring-tasks",
            get(list_rules).post(create_rule),
        )
        .route("/keyholder/recurring-tasks/{id}", patch(patch_rule))
}
