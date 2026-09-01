//! `POST/GET /keyholder/invites`, `DELETE /keyholder/invites/{id}`,
//! `POST /auth/invites/redeem` (03-api-design.md §§1–2).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};

use crate::api::auth::MeResponse;
use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::password;
use crate::auth::session::{self, CurrentUser, Role};
use crate::db::Pool;
use crate::domain::invites;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "invite not found");
const INVALID_INVITE: ApiError = ApiError::new(
    StatusCode::GONE,
    "invalid_invite",
    "invite token is invalid, expired, or already used",
);
const EMAIL_IN_USE: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "email_in_use",
    "an account with that email already exists",
);

#[derive(Deserialize, Default)]
pub struct CreateInviteRequest {
    expires_in_hours: Option<i64>,
}

#[derive(Serialize)]
pub struct CreateInviteResponse {
    token: String,
    expires_at: String,
}

async fn create_invite(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<CreateInviteRequest>,
) -> Result<Json<CreateInviteResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;

    let invite = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = pool.get()?;
        Ok(invites::create(&conn, &user.user_id, req.expires_in_hours)?)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(CreateInviteResponse {
        token: invite.token,
        expires_at: iso8601(invite.expires_at),
    }))
}

#[derive(Serialize)]
pub struct InviteSummary {
    id: String,
    expires_at: String,
    used_at: Option<String>,
    used_by_user_id: Option<String>,
}

async fn list_invites(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<InviteSummary>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;

    let rows = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = pool.get()?;
        Ok(invites::list_for_keyholder(&conn, &user.user_id)?)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(
        rows.into_iter()
            .map(|i| InviteSummary {
                id: i.id,
                expires_at: iso8601(i.expires_at),
                used_at: i.used_at.map(iso8601),
                used_by_user_id: i.used_by_user_id,
            })
            .collect(),
    ))
}

async fn revoke_invite(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(invite_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;

    let revoked = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let conn = pool.get()?;
        Ok(invites::revoke(&conn, &invite_id, &user.user_id)?)
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

#[derive(Deserialize)]
pub struct RedeemInviteRequest {
    token: String,
    email: String,
    password: String,
    display_name: String,
}

async fn redeem_invite(
    State(pool): State<Pool>,
    jar: CookieJar,
    Json(req): Json<RedeemInviteRequest>,
) -> Result<(CookieJar, Json<MeResponse>), ApiError> {
    let password_hash = password::hash_password(&req.password).map_err(|_| INTERNAL_ERROR)?;
    let display_name = req.display_name.clone();

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let mut conn = pool.get()?;
        let redeemed = invites::redeem(
            &mut conn,
            invites::Redeem {
                token: &req.token,
                email: &req.email,
                password_hash: &password_hash,
                display_name: &req.display_name,
            },
        );
        match redeemed {
            Ok(account) => {
                // Auto-login after signup — the docs don't specify this
                // either way, but it's the ordinary "just signed up, now
                // you're in" flow and doesn't relax any stated security
                // property (the same session-creation path a normal
                // login uses).
                let session_id = session::create(&conn, &account.user_id, None)?;
                Ok(Ok((session_id, account.user_id)))
            }
            Err(e) => Ok(Err(e)),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    let (session_id, user_id) = match result {
        Ok(pair) => pair,
        Err(invites::RedeemError::InvalidOrExpired) => return Err(INVALID_INVITE),
        Err(invites::RedeemError::EmailInUse) => return Err(EMAIL_IN_USE),
        Err(invites::RedeemError::Db(_)) => return Err(INTERNAL_ERROR),
    };

    let cookie = crate::api::auth::session_cookie(session_id);
    Ok((
        jar.add(cookie),
        Json(MeResponse {
            id: user_id,
            role: "submissive".to_string(),
            display_name,
        }),
    ))
}

pub fn router() -> Router<Pool> {
    Router::new()
        .route("/keyholder/invites", post(create_invite).get(list_invites))
        .route("/keyholder/invites/{id}", delete(revoke_invite))
        .route("/auth/invites/redeem", post(redeem_invite))
}
