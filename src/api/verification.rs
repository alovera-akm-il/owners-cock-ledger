//! Verification policy & codes (03-api-design.md §5).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db;
use crate::db::Pool;
use crate::domain::links;
use crate::domain::verification::{codes, policy};

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const CONFLICT: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "conflict",
    "request conflicts with current state",
);

fn require_owned_link(
    conn: &rusqlite::Connection,
    keyholder_id: &str,
    submissive_id: &str,
) -> Result<String, ApiError> {
    links::active_or_paused_link_for_keyholder(conn, keyholder_id, submissive_id)
        .map_err(|_| INTERNAL_ERROR)?
        .ok_or(NOT_FOUND)
}

fn require_own_link(conn: &rusqlite::Connection, submissive_id: &str) -> Result<String, ApiError> {
    links::active_link_for_submissive(conn, submissive_id)
        .map_err(|_| INTERNAL_ERROR)?
        .ok_or(NOT_FOUND)
}

#[derive(Serialize)]
struct PolicyResponse {
    frequency_kind: String,
    frequency_value: serde_json::Value,
    code_ttl_seconds: i64,
    grace_period_seconds: i64,
    updated_at: String,
}

impl From<policy::Policy> for PolicyResponse {
    fn from(p: policy::Policy) -> Self {
        let frequency_value =
            serde_json::from_str(&p.frequency_value).unwrap_or(serde_json::Value::Null);
        Self {
            frequency_kind: p.frequency_kind,
            frequency_value,
            code_ttl_seconds: p.code_ttl_seconds,
            grace_period_seconds: p.grace_period_seconds,
            updated_at: iso8601(p.updated_at),
        }
    }
}

async fn get_policy_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<PolicyResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = require_owned_link(&conn, &user.user_id, &submissive_id)?;
        let p = policy::get_for_link(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        Ok(Json(p.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct SetPolicyRequest {
    frequency_kind: String,
    frequency_value: serde_json::Value,
    code_ttl_seconds: i64,
    grace_period_seconds: i64,
}

async fn put_policy(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<SetPolicyRequest>,
) -> Result<Json<PolicyResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:verification-policy")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = require_owned_link(&conn, &user.user_id, &submissive_id)?;
        policy::set_for_link(
            &conn,
            &link_id,
            policy::SetPolicy {
                frequency_kind: &req.frequency_kind,
                frequency_value: &req.frequency_value.to_string(),
                code_ttl_seconds: req.code_ttl_seconds,
                grace_period_seconds: req.grace_period_seconds,
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        let p = policy::get_for_link(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        Ok(Json(p.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn own_policy(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<PolicyResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = require_own_link(&conn, &user.user_id)?;
        let p = policy::get_for_link(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        Ok(Json(p.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Serialize)]
struct CodeResponse {
    code: String,
    issued_at: String,
    expires_at: String,
}

impl From<codes::Code> for CodeResponse {
    fn from(c: codes::Code) -> Self {
        Self {
            code: c.code,
            issued_at: iso8601(c.issued_at),
            expires_at: iso8601(c.expires_at),
        }
    }
}

async fn current_code(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Option<CodeResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = require_own_link(&conn, &user.user_id)?;
        let code = codes::current_unconsumed(&conn, &link_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(code.map(Into::into)))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn request_code(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<CodeResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = require_own_link(&conn, &user.user_id)?;
        let p = policy::get_for_link(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        match codes::request_on_demand(&conn, &p) {
            Ok(code) => Ok(Json(code.into())),
            Err(codes::RequestError::AlreadyHaveOne) => Err(CONFLICT),
            Err(codes::RequestError::NotAllowed) => Err(CONFLICT),
            Err(codes::RequestError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Serialize)]
struct CodeHistoryEntry {
    code: String,
    issued_at: String,
    expires_at: String,
    consumed_at: Option<String>,
}

async fn code_history(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<Vec<CodeHistoryEntry>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = require_owned_link(&conn, &user.user_id, &submissive_id)?;
        let history = codes::history_for_link(&conn, &link_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(
            history
                .into_iter()
                .map(|h| CodeHistoryEntry {
                    code: h.code,
                    issued_at: iso8601(h.issued_at),
                    expires_at: iso8601(h.expires_at),
                    consumed_at: h.consumed_at.map(iso8601),
                })
                .collect(),
        ))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/keyholder/submissives/{id}/verification-policy",
            get(get_policy_for_keyholder).put(put_policy),
        )
        .route("/submissive/verification-policy", get(own_policy))
        .route("/submissive/verification-codes/current", get(current_code))
        .route(
            "/submissive/verification-codes",
            axum::routing::post(request_code),
        )
        .route(
            "/keyholder/submissives/{id}/verification-codes",
            get(code_history),
        )
}
