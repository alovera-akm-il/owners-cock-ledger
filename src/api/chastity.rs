//! Chastity devices and confinement sessions (03-api-design.md §4).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db;
use crate::db::Pool;
use crate::domain::chastity::{confinement, devices};
use crate::domain::links;
use crate::notify;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const CONFLICT: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "conflict",
    "request conflicts with current state",
);

/// Resolves `submissive_id` to the calling Keyholder's own link to them,
/// 404ing rather than 403ing so the response can't be used to confirm
/// another Keyholder's submissive exists (02-roles-and-permissions.md §1
/// principle 2, 03-api-design.md conventions).
fn require_owned_submissive(
    conn: &rusqlite::Connection,
    keyholder_id: &str,
    submissive_id: &str,
) -> Result<(), ApiError> {
    match links::active_or_paused_link_for_keyholder(conn, keyholder_id, submissive_id) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(NOT_FOUND),
        Err(_) => Err(INTERNAL_ERROR),
    }
}

#[derive(Serialize)]
struct DeviceResponse {
    id: String,
    name: String,
    description: Option<String>,
    added_at: String,
    retired_at: Option<String>,
}

impl From<devices::Device> for DeviceResponse {
    fn from(d: devices::Device) -> Self {
        Self {
            id: d.id,
            name: d.name,
            description: d.description,
            added_at: iso8601(d.added_at),
            retired_at: d.retired_at.map(iso8601),
        }
    }
}

#[derive(Deserialize)]
struct CreateDeviceRequest {
    name: String,
    description: Option<String>,
}

async fn list_devices_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<Vec<DeviceResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        let list = devices::list(&conn, &submissive_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn add_device(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<CreateDeviceRequest>,
) -> Result<Json<DeviceResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        let id = devices::add(&conn, &submissive_id, &req.name, req.description.as_deref())
            .map_err(|_| INTERNAL_ERROR)?;
        let list = devices::list(&conn, &submissive_id).map_err(|_| INTERNAL_ERROR)?;
        let device = list
            .into_iter()
            .find(|d| d.id == id)
            .ok_or(INTERNAL_ERROR)?;
        Ok(Json(device.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct RetireDeviceRequest {
    retired: bool,
}

async fn patch_device(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path((submissive_id, device_id)): Path<(String, String)>,
    Json(req): Json<RetireDeviceRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        if !req.retired {
            return Ok(StatusCode::NO_CONTENT);
        }
        let ok = devices::retire(&conn, &device_id, &submissive_id).map_err(|_| INTERNAL_ERROR)?;
        if ok {
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(NOT_FOUND)
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn list_own_devices(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<DeviceResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list = devices::list(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Serialize)]
struct StatusResponse {
    locked: bool,
    session_id: Option<String>,
    device_id: Option<String>,
    started_at: Option<String>,
    target_release_at: Option<String>,
    time_remaining_seconds: Option<i64>,
    overdue: bool,
    clock_paused: bool,
    clock_pause_message: Option<String>,
}

fn status_response(status: confinement::Status) -> StatusResponse {
    StatusResponse {
        locked: status.locked,
        session_id: status.session.as_ref().map(|s| s.id.clone()),
        device_id: status.session.as_ref().map(|s| s.device_id.clone()),
        started_at: status.session.as_ref().map(|s| iso8601(s.started_at)),
        target_release_at: status
            .session
            .as_ref()
            .and_then(|s| s.target_release_at)
            .map(iso8601),
        time_remaining_seconds: status.time_remaining_seconds,
        overdue: status.overdue,
        clock_paused: status.clock_paused,
        clock_pause_message: status
            .session
            .as_ref()
            .and_then(|s| s.clock_pause_message.clone()),
    }
}

async fn status_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<StatusResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        let status = confinement::status_for(&conn, &submissive_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(status_response(status)))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn own_status(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<StatusResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let status = confinement::status_for(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(status_response(status)))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct StartSessionRequest {
    device_id: String,
    started_reason: String,
    target_release_at: Option<i64>,
    notes: Option<String>,
}

async fn start_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<StartSessionRequest>,
) -> Result<Json<StatusResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        let result = confinement::start(
            &conn,
            confinement::StartSession {
                submissive_id: &submissive_id,
                device_id: &req.device_id,
                started_reason: &req.started_reason,
                target_release_at: req.target_release_at,
                notes: req.notes.as_deref(),
            },
        );
        match result {
            Ok(_) => {}
            Err(confinement::StartError::AlreadyOpen) => return Err(CONFLICT),
            Err(confinement::StartError::Db(_)) => return Err(INTERNAL_ERROR),
        }
        let status = confinement::status_for(&conn, &submissive_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(status_response(status)))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct EndSessionRequest {
    ended_reason: String,
    notes: Option<String>,
}

async fn end_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path((submissive_id, _session_id)): Path<(String, String)>,
    Json(req): Json<EndSessionRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        match confinement::end(
            &conn,
            &submissive_id,
            &req.ended_reason,
            &user.user_id,
            req.notes.as_deref(),
        ) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(confinement::NoOpenSessionError::NotOpen) => Err(CONFLICT),
            Err(confinement::NoOpenSessionError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize, Default)]
struct PauseRequest {
    message: Option<String>,
}

async fn pause_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path((submissive_id, _session_id)): Path<(String, String)>,
    Json(req): Json<PauseRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let submissive_id2 = submissive_id.clone();
    let message = req.message.clone();
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        match confinement::pause(&conn, &submissive_id, req.message.as_deref()) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(confinement::PauseError::NotOpenOrAlreadyPaused) => Err(CONFLICT),
            Err(confinement::PauseError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let body = message.unwrap_or_else(|| "Your lock timer has been paused.".to_string());
    let _ = notify::notify(
        &pool2,
        notify::Event {
            user_id: &submissive_id2,
            link_id: None,
            notification_type: "confinement.clocks_paused",
            title: "Your lock timer was paused",
            body: Some(&body),
            link_path: Some("/submissive"),
            related_entity_type: None,
            related_entity_id: None,
            push: true,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PauseMessageRequest {
    message: String,
}

async fn update_pause_message(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path((submissive_id, _session_id)): Path<(String, String)>,
    Json(req): Json<PauseMessageRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    let message = if req.message.is_empty() {
        None
    } else {
        Some(req.message)
    };
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        match confinement::update_pause_message(&conn, &submissive_id, message.as_deref()) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(confinement::NotPausedError::NotPaused) => Err(CONFLICT),
            Err(confinement::NotPausedError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn resume_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path((submissive_id, _session_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let submissive_id2 = submissive_id.clone();
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let mut conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        match confinement::resume(&mut conn, &submissive_id, &user.user_id) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(confinement::NotPausedError::NotPaused) => Err(CONFLICT),
            Err(confinement::NotPausedError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool2,
        notify::Event {
            user_id: &submissive_id2,
            link_id: None,
            notification_type: "confinement.clocks_resumed",
            title: "Your lock timer resumed",
            body: Some("Your release date just moved forward by the pause length."),
            link_path: Some("/submissive"),
            related_entity_type: None,
            related_entity_id: None,
            push: true,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct TimerRequest {
    delta_seconds: i64,
    notes: Option<String>,
}

async fn adjust_timer(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path((submissive_id, _session_id)): Path<(String, String)>,
    Json(req): Json<TimerRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let submissive_id2 = submissive_id.clone();
    let delta_seconds = req.delta_seconds;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let mut conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        match confinement::adjust_timer(
            &mut conn,
            &submissive_id,
            req.delta_seconds,
            &user.user_id,
            req.notes.as_deref(),
        ) {
            Ok(_) => Ok(StatusCode::NO_CONTENT),
            Err(confinement::NoOpenSessionError::NotOpen) => Err(CONFLICT),
            Err(confinement::NoOpenSessionError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let (title, push) = if delta_seconds >= 0 {
        ("Your lock timer was extended", true)
    } else {
        ("Your lock timer was reduced", false)
    };
    let _ = notify::notify(
        &pool2,
        notify::Event {
            user_id: &submissive_id2,
            link_id: None,
            notification_type: "confinement.adjusted",
            title,
            body: None,
            link_path: Some("/submissive"),
            related_entity_type: None,
            related_entity_id: None,
            push,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct AdjustmentResponse {
    id: String,
    delta_seconds: i64,
    reason: String,
    adjusted_by_user_id: Option<String>,
    adjusted_at: String,
    notes: Option<String>,
    keyholder_reviewed_at: Option<String>,
}

impl From<confinement::Adjustment> for AdjustmentResponse {
    fn from(a: confinement::Adjustment) -> Self {
        Self {
            id: a.id,
            delta_seconds: a.delta_seconds,
            reason: a.reason,
            adjusted_by_user_id: a.adjusted_by_user_id,
            adjusted_at: iso8601(a.adjusted_at),
            notes: a.notes,
            keyholder_reviewed_at: a.keyholder_reviewed_at.map(iso8601),
        }
    }
}

async fn list_timer_adjustments_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path((submissive_id, session_id)): Path<(String, String)>,
) -> Result<Json<Vec<AdjustmentResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        let list = confinement::list_adjustments(&conn, &session_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn list_timer_adjustments_submissive(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<AdjustmentResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        // Confirm the session actually belongs to the caller before
        // returning its adjustment history.
        let owns: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM confinement_sessions WHERE id = ?1 AND submissive_id = ?2",
                rusqlite::params![session_id, user.user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| INTERNAL_ERROR)?;
        if owns.is_none() {
            return Err(NOT_FOUND);
        }
        let list = confinement::list_adjustments(&conn, &session_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn review_timer_adjustment(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path((submissive_id, _session_id, adjustment_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        match confinement::review_adjustment(&conn, &adjustment_id) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(confinement::ReviewAdjustmentError::NotReviewable) => Err(CONFLICT),
            Err(confinement::ReviewAdjustmentError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Serialize)]
struct SessionHistoryEntry {
    id: String,
    device_id: String,
    started_at: String,
    ended_at: Option<String>,
    target_release_at: Option<String>,
    started_reason: String,
    ended_reason: Option<String>,
    notes: Option<String>,
}

impl From<confinement::Session> for SessionHistoryEntry {
    fn from(s: confinement::Session) -> Self {
        Self {
            id: s.id,
            device_id: s.device_id,
            started_at: iso8601(s.started_at),
            ended_at: s.ended_at.map(iso8601),
            target_release_at: s.target_release_at.map(iso8601),
            started_reason: s.started_reason,
            ended_reason: s.ended_reason,
            notes: s.notes,
        }
    }
}

async fn session_history_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<Vec<SessionHistoryEntry>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_owned_submissive(&conn, &user.user_id, &submissive_id)?;
        let list = confinement::history(&conn, &submissive_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn own_session_history(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<SessionHistoryEntry>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list = confinement::history(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// Only reachable when `self_report_allowed` is set on the caller's own
/// active link (03-api-design.md §4) — off by default, since the
/// Keyholder is the system of record for lock/unlock events unless they
/// opt a specific submissive in (01-data-model.md §3).
fn require_self_report_allowed(
    conn: &rusqlite::Connection,
    submissive_id: &str,
) -> Result<(), ApiError> {
    let link_id = links::active_link_for_submissive(conn, submissive_id)
        .map_err(|_| INTERNAL_ERROR)?
        .ok_or(NOT_FOUND)?;
    let settings = links::settings_for_link(conn, &link_id).map_err(|_| INTERNAL_ERROR)?;
    if settings.self_report_allowed {
        Ok(())
    } else {
        Err(FORBIDDEN)
    }
}

/// `POST /submissive/confinement-sessions` — same shape as the
/// Keyholder-side start; the timer-adjustment endpoints stay
/// Keyholder-only even with self-report enabled (self-report covers "I
/// put it back on," not "how long I'm supposed to stay in it").
async fn submissive_start_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<StartSessionRequest>,
) -> Result<Json<StatusResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_self_report_allowed(&conn, &user.user_id)?;
        let result = confinement::start(
            &conn,
            confinement::StartSession {
                submissive_id: &user.user_id,
                device_id: &req.device_id,
                started_reason: &req.started_reason,
                target_release_at: req.target_release_at,
                notes: req.notes.as_deref(),
            },
        );
        match result {
            Ok(_) => {}
            Err(confinement::StartError::AlreadyOpen) => return Err(CONFLICT),
            Err(confinement::StartError::Db(_)) => return Err(INTERNAL_ERROR),
        }
        let status = confinement::status_for(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(status_response(status)))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `PATCH /submissive/confinement-sessions/{sessionId}` — same shape as
/// the Keyholder-side close.
async fn submissive_end_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(_session_id): Path<String>,
    Json(req): Json<EndSessionRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        require_self_report_allowed(&conn, &user.user_id)?;
        match confinement::end(
            &conn,
            &user.user_id,
            &req.ended_reason,
            &user.user_id,
            req.notes.as_deref(),
        ) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(confinement::NoOpenSessionError::NotOpen) => Err(CONFLICT),
            Err(confinement::NoOpenSessionError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize, Default)]
struct OversightPauseRequest {
    message: Option<String>,
}

/// `POST /keyholder/submissives/{id}/oversight-pause`
/// (06-future-extensions.md §13) — directly mirrors the confinement
/// pause endpoint shape, one level up in scope: freezes the deadline
/// sweeper's auto-fail pass and new verification-code issuance for
/// this whole link, cascading into the confinement pause too if
/// there's an open session.
async fn oversight_pause(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<OversightPauseRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let submissive_id2 = submissive_id.clone();
    let message = req.message.clone();
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        match links::pause_oversight(&conn, &user.user_id, &submissive_id, req.message.as_deref()) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(links::OversightPauseError::NoLink) => Err(NOT_FOUND),
            Err(links::OversightPauseError::AlreadyPaused) => Err(CONFLICT),
            Err(links::OversightPauseError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let body = message.unwrap_or_else(|| {
        "Your Keyholder has paused deadlines and verification for a while.".to_string()
    });
    let _ = notify::notify(
        &pool2,
        notify::Event {
            user_id: &submissive_id2,
            link_id: None,
            notification_type: "oversight.paused",
            title: "Oversight has been paused",
            body: Some(&body),
            link_path: Some("/submissive"),
            related_entity_type: None,
            related_entity_id: None,
            push: true,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct OversightResumeResponse {
    shifted_assignment_count: i64,
    elapsed_seconds: i64,
}

async fn oversight_resume(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<OversightResumeResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let submissive_id2 = submissive_id.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        match links::resume_oversight(&conn, &user.user_id, &submissive_id, &user.user_id) {
            Ok(outcome) => Ok(outcome),
            Err(links::OversightResumeError::NoLink) => Err(NOT_FOUND),
            Err(links::OversightResumeError::NotPaused) => Err(CONFLICT),
            Err(links::OversightResumeError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool2,
        notify::Event {
            user_id: &submissive_id2,
            link_id: None,
            notification_type: "oversight.resumed",
            title: "Oversight has resumed",
            body: Some("Open deadlines were shifted forward by the pause length."),
            link_path: Some("/submissive"),
            related_entity_type: None,
            related_entity_id: None,
            push: true,
        },
    )
    .await;

    Ok(Json(OversightResumeResponse {
        shifted_assignment_count: outcome.shifted_assignment_count,
        elapsed_seconds: outcome.elapsed_seconds,
    }))
}

#[derive(Deserialize)]
struct OversightPauseMessageRequest {
    message: String,
}

async fn update_oversight_pause_message(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<OversightPauseMessageRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:chastity")
        .map_err(|_| FORBIDDEN)?;
    let message = if req.message.is_empty() {
        None
    } else {
        Some(req.message)
    };
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        match links::update_oversight_pause_message(
            &conn,
            &user.user_id,
            &submissive_id,
            message.as_deref(),
        ) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(links::OversightResumeError::NoLink) => Err(NOT_FOUND),
            Err(links::OversightResumeError::NotPaused) => Err(CONFLICT),
            Err(links::OversightResumeError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/keyholder/submissives/{id}/devices",
            get(list_devices_for_keyholder).post(add_device),
        )
        .route("/keyholder/submissives/{id}/devices/{deviceId}", patch(patch_device))
        .route("/submissive/devices", get(list_own_devices))
        .route("/keyholder/submissives/{id}/status", get(status_for_keyholder))
        .route("/submissive/status", get(own_status))
        .route(
            "/keyholder/submissives/{id}/confinement-sessions",
            get(session_history_for_keyholder).post(start_session),
        )
        .route(
            "/keyholder/submissives/{id}/confinement-sessions/{sessionId}",
            patch(end_session),
        )
        .route(
            "/keyholder/submissives/{id}/confinement-sessions/{sessionId}/pause",
            post(pause_session),
        )
        .route(
            "/keyholder/submissives/{id}/confinement-sessions/{sessionId}/pause-message",
            patch(update_pause_message),
        )
        .route(
            "/keyholder/submissives/{id}/confinement-sessions/{sessionId}/resume",
            post(resume_session),
        )
        .route(
            "/keyholder/submissives/{id}/confinement-sessions/{sessionId}/timer",
            patch(adjust_timer),
        )
        .route(
            "/keyholder/submissives/{id}/confinement-sessions/{sessionId}/timer-adjustments",
            get(list_timer_adjustments_keyholder),
        )
        .route(
            "/keyholder/submissives/{id}/confinement-sessions/{sessionId}/timer-adjustments/{adjustmentId}/review",
            patch(review_timer_adjustment),
        )
        .route(
            "/keyholder/submissives/{id}/oversight-pause",
            post(oversight_pause),
        )
        .route(
            "/keyholder/submissives/{id}/oversight-resume",
            post(oversight_resume),
        )
        .route(
            "/keyholder/submissives/{id}/oversight-pause-message",
            patch(update_oversight_pause_message),
        )
        .route(
            "/submissive/confinement-sessions/{sessionId}/timer-adjustments",
            get(list_timer_adjustments_submissive),
        )
        .route(
            "/submissive/confinement-sessions",
            get(own_session_history).post(submissive_start_session),
        )
        .route(
            "/submissive/confinement-sessions/{sessionId}",
            patch(submissive_end_session),
        )
}
