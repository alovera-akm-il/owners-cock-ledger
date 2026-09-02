//! `POST /auth/login`, `POST /auth/logout`, `GET /auth/me`
//! (03-api-design.md §1). 2FA is Phase 4 (`10-operations.md` §2) — every
//! login here is single-factor, so the `requires_2fa` branch the docs
//! describe doesn't exist yet.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::password;
use crate::auth::session::{self, CurrentUser, SESSION_COOKIE_NAME};
use crate::db;
use crate::db::Pool;
use crate::domain::users;

const INVALID_CREDENTIALS: ApiError = ApiError::new(
    StatusCode::UNAUTHORIZED,
    "invalid_credentials",
    "invalid email or password",
);

const INCORRECT_PASSWORD: ApiError = ApiError::new(
    StatusCode::UNAUTHORIZED,
    "incorrect_password",
    "current password is incorrect",
);

const SESSION_NOT_FOUND: ApiError = ApiError::new(
    StatusCode::NOT_FOUND,
    "session_not_found",
    "no such active session",
);

#[derive(Deserialize)]
pub struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub id: String,
    pub role: String,
    pub display_name: String,
}

enum LoginOutcome {
    Invalid,
    Success {
        session_id: String,
        account: MeResponse,
    },
}

pub fn session_cookie(session_id: String) -> Cookie<'static> {
    // HttpOnly, Secure, SameSite=Strict, matching 05-security-and-privacy.md
    // §2 exactly.
    Cookie::build((SESSION_COOKIE_NAME, session_id))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .build()
}

async fn login(
    State(pool): State<Pool>,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> Result<(CookieJar, Json<MeResponse>), ApiError> {
    let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<LoginOutcome> {
        let conn = pool.get()?;
        let account = users::find_by_email(&conn, &req.email)?;

        // Always run the verification hash, even for a nonexistent email
        // (against the fixed dummy hash) — the timing must be
        // indistinguishable from a real wrong-password attempt
        // (05-security-and-privacy.md §2).
        let hash_to_check: &str = match &account {
            Some(a) => &a.password_hash,
            None => password::dummy_hash(),
        };
        let password_ok = password::verify_password(&req.password, hash_to_check);

        let Some(account) = account else {
            return Ok(LoginOutcome::Invalid);
        };
        if account.disabled_at.is_some() {
            return Ok(LoginOutcome::Invalid);
        }
        if users::is_locked(&account, session::now()) {
            return Ok(LoginOutcome::Invalid);
        }
        if !password_ok {
            users::record_failed_login(&conn, &account.id)?;
            return Ok(LoginOutcome::Invalid);
        }

        users::record_successful_login(&conn, &account.id)?;
        let session_id = session::create(&conn, &account.id, None)?;
        Ok(LoginOutcome::Success {
            session_id,
            account: MeResponse {
                id: account.id,
                role: account.role,
                display_name: account.display_name,
            },
        })
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    match outcome {
        LoginOutcome::Invalid => Err(INVALID_CREDENTIALS),
        LoginOutcome::Success {
            session_id,
            account,
        } => Ok((jar.add(session_cookie(session_id)), Json(account))),
    }
}

async fn logout(
    State(pool): State<Pool>,
    jar: CookieJar,
    user: CurrentUser,
) -> Result<CookieJar, ApiError> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = pool.get()?;
        session::revoke(&conn, &user.session_id)?;
        Ok(())
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    let removal = Cookie::build((SESSION_COOKIE_NAME, "")).path("/").build();
    Ok(jar.remove(removal))
}

async fn me(user: CurrentUser) -> Json<MeResponse> {
    Json(MeResponse {
        id: user.user_id,
        role: match user.role {
            session::Role::Keyholder => "keyholder".to_string(),
            session::Role::Submissive => "submissive".to_string(),
        },
        display_name: user.display_name,
    })
}

#[derive(Deserialize)]
struct PasswordChangeRequest {
    current_password: String,
    new_password: String,
}

/// `POST /auth/password/change` (03-api-design.md §1): revokes every
/// *other* session for this user in the same transaction as the password
/// update — a password change is exactly the moment to assume an old
/// session might be compromised (10-operations.md §1).
async fn change_password(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<PasswordChangeRequest>,
) -> Result<StatusCode, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        let mut conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let current_hash: String = conn
            .query_row(
                "SELECT password_hash FROM users WHERE id = ?1",
                rusqlite::params![user.user_id],
                |row| row.get(0),
            )
            .map_err(|_| INTERNAL_ERROR)?;

        if !password::verify_password(&req.current_password, &current_hash) {
            return Err(INCORRECT_PASSWORD);
        }

        let new_hash = password::hash_password(&req.new_password).map_err(|_| INTERNAL_ERROR)?;
        let tx = conn.transaction().map_err(|_| INTERNAL_ERROR)?;
        tx.execute(
            "UPDATE users SET password_hash = ?1 WHERE id = ?2",
            rusqlite::params![new_hash, user.user_id],
        )
        .map_err(|_| INTERNAL_ERROR)?;
        session::revoke_all_except(&tx, &user.user_id, &user.session_id)
            .map_err(|_| INTERNAL_ERROR)?;
        tx.commit().map_err(|_| INTERNAL_ERROR)?;
        Ok(())
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct SessionSummaryResponse {
    id: String,
    created_at: String,
    last_seen_at: String,
    user_agent: Option<String>,
    is_current: bool,
}

/// `GET /auth/sessions` (10-operations.md §1) — "where am I logged in".
async fn list_sessions(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<SessionSummaryResponse>>, ApiError> {
    let current_session_id = user.session_id.clone();
    let sessions = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = pool.get()?;
        session::list_for_user(&conn, &user.user_id).map_err(Into::into)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(
        sessions
            .into_iter()
            .map(|s| SessionSummaryResponse {
                is_current: s.id == current_session_id,
                id: s.id,
                created_at: iso8601(s.created_at),
                last_seen_at: iso8601(s.last_seen_at),
                user_agent: s.user_agent,
            })
            .collect(),
    ))
}

/// `DELETE /auth/sessions/{id}` — self-scoped; revoking the caller's own
/// current session is allowed and behaves like logout (03-api-design.md
/// §1), but doesn't clear the cookie itself since the client already
/// knows which session it just revoked.
async fn delete_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(session_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let revoked = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let conn = pool.get()?;
        session::revoke_own(&conn, &session_id, &user.user_id).map_err(Into::into)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    if revoked {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(SESSION_NOT_FOUND)
    }
}

#[derive(Deserialize)]
struct RevokeSessionsRequest {
    #[serde(default = "default_true")]
    except_current: bool,
}

fn default_true() -> bool {
    true
}

/// `DELETE /auth/sessions` — "log out everywhere else" in one action
/// (10-operations.md §1). `except_current` only ever means "keep the
/// caller's own session" — there's no documented use for revoking
/// literally everything including the session making the request, so
/// `false` is treated the same as `true`.
async fn revoke_sessions(
    State(pool): State<Pool>,
    user: CurrentUser,
    body: Option<Json<RevokeSessionsRequest>>,
) -> Result<StatusCode, ApiError> {
    // `except_current` only ever means "keep the caller's own session" —
    // there's no documented behavior for `false`, so both values take the
    // same action; the field still round-trips through the request shape
    // rather than being rejected as unknown.
    let _except_current = body.map(|Json(b)| b.except_current).unwrap_or(true);
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = pool.get()?;
        session::revoke_all_except(&conn, &user.user_id, &user.session_id)?;
        Ok(())
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/password/change", post(change_password))
        .route("/auth/sessions", get(list_sessions).delete(revoke_sessions))
        .route("/auth/sessions/{id}", delete(delete_session))
}
