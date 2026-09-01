//! `POST /auth/login`, `POST /auth/logout`, `GET /auth/me`
//! (03-api-design.md §1). 2FA is Phase 4 (`10-operations.md` §2) — every
//! login here is single-factor, so the `requires_2fa` branch the docs
//! describe doesn't exist yet.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR};
use crate::auth::password;
use crate::auth::session::{self, CurrentUser, SESSION_COOKIE_NAME};
use crate::db::Pool;
use crate::domain::users;

const INVALID_CREDENTIALS: ApiError = ApiError::new(
    StatusCode::UNAUTHORIZED,
    "invalid_credentials",
    "invalid email or password",
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

pub fn router() -> Router<Pool> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
}
