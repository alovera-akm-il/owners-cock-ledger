//! `POST/GET /keyholder/api-tokens`, `PATCH/DELETE /keyholder/api-tokens/{id}`
//! (03-api-design.md §12) — Keyholder automation tokens. No scope is
//! required to manage a token, since a Keyholder is only ever creating,
//! narrowing, or revoking access to their *own* account either way,
//! session- or token-authenticated.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db;
use crate::db::Pool;
use crate::domain::api_tokens;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "token not found");

#[derive(Deserialize)]
struct CreateTokenRequest {
    label: String,
    #[serde(default)]
    scopes: Vec<String>,
    expires_in_days: Option<i64>,
}

#[derive(Serialize)]
struct CreateTokenResponse {
    id: String,
    token: String,
    prefix: String,
    expires_at: Option<String>,
}

/// `POST /keyholder/api-tokens` — the full raw token is returned exactly
/// once, in this response only.
async fn create_token(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<CreateTokenRequest>,
) -> Result<Json<CreateTokenResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;

    let created =
        tokio::task::spawn_blocking(move || -> anyhow::Result<api_tokens::CreatedToken> {
            let conn = pool.get()?;
            api_tokens::create(
                &conn,
                &user.user_id,
                &req.label,
                &req.scopes,
                req.expires_in_days,
            )
            .map_err(Into::into)
        })
        .await
        .map_err(|_| INTERNAL_ERROR)?
        .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(CreateTokenResponse {
        id: created.id,
        token: created.token,
        prefix: created.prefix,
        expires_at: created.expires_at.map(iso8601),
    }))
}

#[derive(Serialize)]
struct TokenSummaryResponse {
    id: String,
    label: String,
    token_prefix: String,
    scopes: Vec<String>,
    created_at: String,
    expires_at: Option<String>,
    last_used_at: Option<String>,
    revoked_at: Option<String>,
}

impl From<api_tokens::TokenSummary> for TokenSummaryResponse {
    fn from(t: api_tokens::TokenSummary) -> Self {
        TokenSummaryResponse {
            id: t.id,
            label: t.label,
            token_prefix: t.token_prefix,
            scopes: t.scopes,
            created_at: iso8601(t.created_at),
            expires_at: t.expires_at.map(iso8601),
            last_used_at: t.last_used_at.map(iso8601),
            revoked_at: t.revoked_at.map(iso8601),
        }
    }
}

/// `GET /keyholder/api-tokens` — never returns the full token value.
async fn list_tokens(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<TokenSummaryResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;

    let tokens = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = pool.get()?;
        api_tokens::list_for_keyholder(&conn, &user.user_id).map_err(Into::into)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(tokens.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize)]
struct UpdateTokenRequest {
    label: Option<String>,
    scopes: Option<Vec<String>>,
}

/// `PATCH /keyholder/api-tokens/{id}` — narrowing or renaming without
/// rotating; widening scopes is allowed too.
async fn update_token(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(token_id): Path<String>,
    Json(req): Json<UpdateTokenRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;

    let updated = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let conn = pool.get()?;
        api_tokens::update(
            &conn,
            &token_id,
            &user.user_id,
            req.label.as_deref(),
            req.scopes.as_deref(),
        )
        .map_err(Into::into)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    if updated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(NOT_FOUND)
    }
}

/// `DELETE /keyholder/api-tokens/{id}` — revoke: a revoked token fails
/// auth on its very next request.
async fn revoke_token(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(token_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;

    let revoked = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let conn = pool.get()?;
        api_tokens::revoke(&conn, &token_id, &user.user_id).map_err(Into::into)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    if revoked {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(NOT_FOUND)
    }
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/keyholder/api-tokens", post(create_token).get(list_tokens))
        .route(
            "/keyholder/api-tokens/{id}",
            patch(update_token).delete(revoke_token),
        )
}
