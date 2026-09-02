//! `POST /auth/login`, `POST /auth/logout`, `GET /auth/me`, session
//! self-management, two-factor authentication, and password reset
//! (03-api-design.md §1).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::password;
use crate::auth::session::{self, CurrentUser, SESSION_COOKIE_NAME};
use crate::db;
use crate::db::Pool;
use crate::domain::{password_reset, two_factor, users};

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

const INVALID_TWO_FACTOR_CODE: ApiError = ApiError::new(
    StatusCode::UNAUTHORIZED,
    "invalid_code",
    "that code didn't match",
);

const NO_PENDING_TWO_FACTOR_SETUP: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "no_pending_setup",
    "no pending 2FA setup to confirm",
);

const TWO_FACTOR_NOT_ENABLED: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "two_factor_not_enabled",
    "2FA is not enabled on this account",
);

const CHALLENGE_EXPIRED: ApiError = ApiError::new(
    StatusCode::GONE,
    "challenge_expired",
    "login challenge expired or exceeded its attempt limit — log in again",
);

const INVALID_RESET_TOKEN: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "invalid_reset_token",
    "reset token is invalid, expired, or already used",
);

/// Session self-management (10-operations.md §1) is inherently about
/// interactive login sessions — an API token has nothing analogous to
/// manage, so every endpoint in that group rejects token auth outright
/// rather than doing something token-shaped-but-wrong with no session id.
const SESSION_AUTH_REQUIRED: ApiError = ApiError::new(
    StatusCode::FORBIDDEN,
    "session_required",
    "this action requires an interactive login session, not an API token",
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
    RequiresTwoFactor {
        challenge_token: String,
    },
    Success {
        session_id: String,
        account: MeResponse,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
enum LoginResponseBody {
    Challenge {
        requires_2fa: bool,
        challenge_token: String,
    },
    Success(MeResponse),
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
) -> Result<(CookieJar, Json<LoginResponseBody>), ApiError> {
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

        // The password was necessary but not sufficient — a confirmed
        // second factor means login isn't done yet (03-api-design.md §1).
        if two_factor::is_enabled(&conn, &account.id)? {
            let challenge_token = two_factor::create_challenge(&conn, &account.id)?;
            return Ok(LoginOutcome::RequiresTwoFactor { challenge_token });
        }

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
        LoginOutcome::RequiresTwoFactor { challenge_token } => Ok((
            jar,
            Json(LoginResponseBody::Challenge {
                requires_2fa: true,
                challenge_token,
            }),
        )),
        LoginOutcome::Success {
            session_id,
            account,
        } => Ok((
            jar.add(session_cookie(session_id)),
            Json(LoginResponseBody::Success(account)),
        )),
    }
}

#[derive(Deserialize)]
struct TwoFactorVerifyRequest {
    challenge_token: String,
    code: String,
}

/// `POST /auth/2fa/verify` — completes a login that `POST /auth/login`
/// left half-finished with a `challenge_token` (03-api-design.md §1).
/// Accepts either a live TOTP code or a recovery code.
async fn verify_two_factor(
    State(pool): State<Pool>,
    jar: CookieJar,
    Json(req): Json<TwoFactorVerifyRequest>,
) -> Result<(CookieJar, Json<MeResponse>), ApiError> {
    let outcome = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Result<(String, MeResponse), two_factor::ChallengeVerifyError>> {
            let conn = pool.get()?;
            let user_id = match two_factor::verify_challenge(
                &conn,
                &req.challenge_token,
                |uid| {
                    conn.query_row(
                        "SELECT email FROM users WHERE id = ?1",
                        params![uid],
                        |row| row.get(0),
                    )
                },
                &req.code,
            ) {
                Ok(user_id) => user_id,
                Err(e) => return Ok(Err(e)),
            };

            let account: MeResponse = conn.query_row(
                "SELECT id, role, display_name FROM users WHERE id = ?1",
                params![user_id],
                |row| {
                    Ok(MeResponse {
                        id: row.get(0)?,
                        role: row.get(1)?,
                        display_name: row.get(2)?,
                    })
                },
            )?;
            let session_id = session::create(&conn, &user_id, None)?;
            Ok(Ok((session_id, account)))
        },
    )
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    match outcome {
        Ok((session_id, account)) => Ok((jar.add(session_cookie(session_id)), Json(account))),
        Err(two_factor::ChallengeVerifyError::ExpiredOrExhausted) => Err(CHALLENGE_EXPIRED),
        Err(_) => Err(INVALID_TWO_FACTOR_CODE),
    }
}

async fn logout(
    State(pool): State<Pool>,
    jar: CookieJar,
    user: CurrentUser,
) -> Result<CookieJar, ApiError> {
    let session_id = user.session_id().ok_or(SESSION_AUTH_REQUIRED)?.to_string();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = pool.get()?;
        session::revoke(&conn, &session_id)?;
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
    let session_id = user.session_id().ok_or(SESSION_AUTH_REQUIRED)?.to_string();
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
        session::revoke_all_except(&tx, &user.user_id, &session_id).map_err(|_| INTERNAL_ERROR)?;
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
    let current_session_id = user.session_id().ok_or(SESSION_AUTH_REQUIRED)?.to_string();
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
    user.session_id().ok_or(SESSION_AUTH_REQUIRED)?;
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
    let session_id = user.session_id().ok_or(SESSION_AUTH_REQUIRED)?.to_string();
    // `except_current` only ever means "keep the caller's own session" —
    // there's no documented behavior for `false`, so both values take the
    // same action; the field still round-trips through the request shape
    // rather than being rejected as unknown.
    let _except_current = body.map(|Json(b)| b.except_current).unwrap_or(true);
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = pool.get()?;
        session::revoke_all_except(&conn, &user.user_id, &session_id)?;
        Ok(())
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct TwoFactorStatusResponse {
    enabled: bool,
    pending_setup: bool,
    recovery_codes_remaining: i64,
}

/// `GET /auth/2fa/status`.
async fn two_factor_status(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<TwoFactorStatusResponse>, ApiError> {
    let status = tokio::task::spawn_blocking(move || -> anyhow::Result<two_factor::Status> {
        let conn = pool.get()?;
        two_factor::status(&conn, &user.user_id).map_err(Into::into)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(TwoFactorStatusResponse {
        enabled: status.enabled,
        pending_setup: status.pending_setup,
        recovery_codes_remaining: status.recovery_codes_remaining,
    }))
}

#[derive(Serialize)]
struct TwoFactorSetupResponse {
    secret: String,
    otpauth_uri: String,
    qr_png_base64: String,
}

/// `POST /auth/2fa/setup` — no body. Calling this again before confirming
/// replaces the pending secret rather than erroring.
async fn two_factor_setup(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<TwoFactorSetupResponse>, ApiError> {
    let pending =
        tokio::task::spawn_blocking(move || -> anyhow::Result<two_factor::PendingSetup> {
            let conn = pool.get()?;
            let email: String = conn.query_row(
                "SELECT email FROM users WHERE id = ?1",
                params![&user.user_id],
                |row| row.get(0),
            )?;
            two_factor::setup(&conn, &user.user_id, &email)
        })
        .await
        .map_err(|_| INTERNAL_ERROR)?
        .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(TwoFactorSetupResponse {
        secret: pending.secret_base32,
        otpauth_uri: pending.otpauth_uri,
        qr_png_base64: pending.qr_png_base64,
    }))
}

#[derive(Deserialize)]
struct TwoFactorCodeRequest {
    code: String,
}

#[derive(Serialize)]
struct RecoveryCodesResponse {
    recovery_codes: Vec<String>,
}

/// `POST /auth/2fa/confirm` — validates `code` against the pending
/// secret from `setup`, then enables 2FA and issues 10 recovery codes,
/// returned exactly once.
async fn two_factor_confirm(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<TwoFactorCodeRequest>,
) -> Result<Json<RecoveryCodesResponse>, ApiError> {
    let outcome = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Result<Vec<String>, two_factor::ConfirmError>> {
            let mut conn = pool.get()?;
            let email: String = conn.query_row(
                "SELECT email FROM users WHERE id = ?1",
                params![&user.user_id],
                |row| row.get(0),
            )?;
            Ok(two_factor::confirm(
                &mut conn,
                &user.user_id,
                &email,
                &req.code,
            ))
        },
    )
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    match outcome {
        Ok(recovery_codes) => Ok(Json(RecoveryCodesResponse { recovery_codes })),
        Err(two_factor::ConfirmError::NoPendingSetup) => Err(NO_PENDING_TWO_FACTOR_SETUP),
        Err(_) => Err(INVALID_TWO_FACTOR_CODE),
    }
}

#[derive(Deserialize)]
struct TwoFactorPasswordAndCodeRequest {
    current_password: String,
    code: String,
}

/// `POST /auth/2fa/disable` — requires both the password and a live
/// code, not just one (10-operations.md §2): a hijacked session already
/// bypasses the password, so the code is what actually gates this.
async fn two_factor_disable(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<TwoFactorPasswordAndCodeRequest>,
) -> Result<StatusCode, ApiError> {
    let outcome: Result<(), ApiError> =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Result<(), ApiError>> {
            let conn = pool.get()?;
            let (email, password_hash): (String, String) = conn.query_row(
                "SELECT email, password_hash FROM users WHERE id = ?1",
                params![&user.user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if !password::verify_password(&req.current_password, &password_hash) {
                return Ok(Err(INCORRECT_PASSWORD));
            }
            match two_factor::disable(&conn, &user.user_id, &email, &req.code) {
                Ok(()) => Ok(Ok(())),
                Err(two_factor::DisableError::NotEnabled) => Ok(Err(TWO_FACTOR_NOT_ENABLED)),
                Err(_) => Ok(Err(INVALID_TWO_FACTOR_CODE)),
            }
        })
        .await
        .map_err(|_| INTERNAL_ERROR)?
        .map_err(|_| INTERNAL_ERROR)?;

    outcome?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /auth/2fa/recovery-codes/regenerate` — same dual-proof
/// requirement as disabling; invalidates every existing recovery code.
async fn two_factor_regenerate_recovery_codes(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<TwoFactorPasswordAndCodeRequest>,
) -> Result<Json<RecoveryCodesResponse>, ApiError> {
    let outcome: Result<Vec<String>, ApiError> =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Result<Vec<String>, ApiError>> {
            let mut conn = pool.get()?;
            let (email, password_hash): (String, String) = conn.query_row(
                "SELECT email, password_hash FROM users WHERE id = ?1",
                params![&user.user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if !password::verify_password(&req.current_password, &password_hash) {
                return Ok(Err(INCORRECT_PASSWORD));
            }
            match two_factor::regenerate_recovery_codes(&mut conn, &user.user_id, &email, &req.code)
            {
                Ok(codes) => Ok(Ok(codes)),
                Err(two_factor::RegenerateError::NotEnabled) => Ok(Err(TWO_FACTOR_NOT_ENABLED)),
                Err(_) => Ok(Err(INVALID_TWO_FACTOR_CODE)),
            }
        })
        .await
        .map_err(|_| INTERNAL_ERROR)?
        .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(RecoveryCodesResponse {
        recovery_codes: outcome?,
    }))
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct PasswordResetRequestRequest {
    email: String,
}

/// `POST /auth/password-reset/request` — always the same generic `202`,
/// regardless of whether the email has an account or outbound email is
/// even configured (05-security-and-privacy.md §11). This deployment
/// hasn't wired up outbound SMTP, so this endpoint is permanently a
/// no-op for now; `admin reset-password` (10-operations.md §5) is the
/// always-available path. The shape exists now so a future SMTP
/// integration only has to fill in the send step, not invent the route.
async fn request_password_reset(Json(_req): Json<PasswordResetRequestRequest>) -> StatusCode {
    StatusCode::ACCEPTED
}

#[derive(Deserialize)]
struct PasswordResetRedeemRequest {
    token: String,
    new_password: String,
}

/// `POST /auth/password-reset/redeem` — public, requires a valid token
/// from either issuance path. Sets the password, consumes the token, and
/// revokes every existing session for the account (10-operations.md §5).
async fn redeem_password_reset(
    State(pool): State<Pool>,
    Json(req): Json<PasswordResetRedeemRequest>,
) -> Result<StatusCode, ApiError> {
    let new_hash = password::hash_password(&req.new_password).map_err(|_| INTERNAL_ERROR)?;
    let outcome = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Result<String, password_reset::RedeemError>> {
            let mut conn = pool.get()?;
            Ok(password_reset::redeem(&mut conn, &req.token, &new_hash))
        },
    )
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    match outcome {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(_) => Err(INVALID_RESET_TOKEN),
    }
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/password/change", post(change_password))
        .route("/auth/sessions", get(list_sessions).delete(revoke_sessions))
        .route("/auth/sessions/{id}", delete(delete_session))
        .route("/auth/2fa/status", get(two_factor_status))
        .route("/auth/2fa/setup", post(two_factor_setup))
        .route("/auth/2fa/confirm", post(two_factor_confirm))
        .route("/auth/2fa/verify", post(verify_two_factor))
        .route("/auth/2fa/disable", post(two_factor_disable))
        .route(
            "/auth/2fa/recovery-codes/regenerate",
            post(two_factor_regenerate_recovery_codes),
        )
        .route("/auth/password-reset/request", post(request_password_reset))
        .route("/auth/password-reset/redeem", post(redeem_password_reset))
}
