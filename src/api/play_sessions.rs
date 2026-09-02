//! Play sessions and their templates (03-api-design.md §10,
//! 14-play-sessions.md). Live start/end can be done by either role for
//! their own link; template management, judgement, completion, and
//! cancellation are Keyholder-only. The check-in SSE stream
//! (`GET /play-sessions/{id}/checkin-stream`) is Phase 7 and
//! deliberately not built here.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, Pool};
use crate::domain::{links, play_sessions};
use crate::notify;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const BAD_REQUEST: ApiError =
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "malformed request");
const CONFLICT: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "conflict",
    "request conflicts with current state",
);

fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn parse_iso8601(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

// ---- Templates ----

#[derive(Serialize)]
struct TemplateResponse {
    id: String,
    title: String,
    setup_notes: Option<String>,
    suggested_toy_categories: Option<serde_json::Value>,
    planned_duration_seconds: Option<i64>,
    checkin_template_id: Option<String>,
    checkin_interval_seconds: Option<i64>,
    active: bool,
    created_at: String,
}

impl From<play_sessions::Template> for TemplateResponse {
    fn from(t: play_sessions::Template) -> Self {
        Self {
            id: t.id,
            title: t.title,
            setup_notes: t.setup_notes,
            suggested_toy_categories: t
                .suggested_toy_categories
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
            planned_duration_seconds: t.planned_duration_seconds,
            checkin_template_id: t.checkin_template_id,
            checkin_interval_seconds: t.checkin_interval_seconds,
            active: t.active,
            created_at: iso8601(t.created_at),
        }
    }
}

async fn list_templates(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<TemplateResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list = play_sessions::list_templates_for_keyholder(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `GET /submissive/play-session-templates` — read-only, gated by
/// `catalog_visible_to_submissive` (same gate as the checkin/reward
/// template lists).
async fn list_templates_for_submissive(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<TemplateResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let settings = links::settings_for_link(&conn, &link_id).map_err(|_| INTERNAL_ERROR)?;
        if !settings.catalog_visible_to_submissive {
            return Ok(Json(Vec::new()));
        }
        let (keyholder_id, _) = links::parties(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        let list = play_sessions::list_templates_for_keyholder(&conn, &keyholder_id)
            .map_err(|_| INTERNAL_ERROR)?
            .into_iter()
            .filter(|t| t.active)
            .map(Into::into)
            .collect();
        Ok(Json(list))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct CreateTemplateRequest {
    title: String,
    setup_notes: Option<String>,
    suggested_toy_categories: Option<serde_json::Value>,
    planned_duration_seconds: Option<i64>,
    checkin_template_id: Option<String>,
    checkin_interval_seconds: Option<i64>,
}

async fn create_template(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<CreateTemplateRequest>,
) -> Result<Json<TemplateResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let suggested = req.suggested_toy_categories.as_ref().map(|v| v.to_string());
        let id = play_sessions::create_template(
            &conn,
            play_sessions::NewTemplate {
                keyholder_id: &user.user_id,
                title: &req.title,
                setup_notes: req.setup_notes.as_deref(),
                suggested_toy_categories: suggested.as_deref(),
                planned_duration_seconds: req.planned_duration_seconds,
                checkin_template_id: req.checkin_template_id.as_deref(),
                checkin_interval_seconds: req.checkin_interval_seconds,
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        let t = play_sessions::get_template(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        Ok(Json(t.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize, Default)]
struct PatchTemplateRequest {
    title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    setup_notes: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    suggested_toy_categories: Option<Option<serde_json::Value>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    planned_duration_seconds: Option<Option<i64>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    checkin_template_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    checkin_interval_seconds: Option<Option<i64>>,
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
        let suggested = req
            .suggested_toy_categories
            .as_ref()
            .map(|v| v.as_ref().map(|j| j.to_string()));
        let updated = play_sessions::update_template(
            &conn,
            &id,
            &user.user_id,
            play_sessions::TemplateEdit {
                title: req.title.as_deref(),
                setup_notes: req.setup_notes.as_ref().map(|v| v.as_deref()),
                suggested_toy_categories: suggested.as_ref().map(|v| v.as_deref()),
                planned_duration_seconds: req.planned_duration_seconds,
                checkin_template_id: req.checkin_template_id.as_ref().map(|v| v.as_deref()),
                checkin_interval_seconds: req.checkin_interval_seconds,
                active: req.active,
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        if !updated {
            return Err(NOT_FOUND);
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

// ---- Sessions ----

#[derive(Serialize)]
struct ScheduleSlotResponse {
    sequence_number: i64,
    planned_offset_seconds: i64,
    checkin_template_id: String,
    fulfilled_checkin_id: Option<String>,
}

impl From<play_sessions::ScheduleSlot> for ScheduleSlotResponse {
    fn from(s: play_sessions::ScheduleSlot) -> Self {
        Self {
            sequence_number: s.sequence_number,
            planned_offset_seconds: s.planned_offset_seconds,
            checkin_template_id: s.checkin_template_id,
            fulfilled_checkin_id: s.fulfilled_checkin_id,
        }
    }
}

#[derive(Serialize)]
struct SessionResponse {
    id: String,
    link_id: String,
    template_id: Option<String>,
    title: String,
    setup_notes: Option<String>,
    status: String,
    planned_duration_seconds: Option<i64>,
    checkin_template_id: Option<String>,
    checkin_interval_seconds: Option<i64>,
    started_at: Option<String>,
    ended_at: Option<String>,
    safety_check_ok: Option<bool>,
    judgement_notes: Option<String>,
    reward_assignment_id: Option<String>,
    punishment_assignment_id: Option<String>,
    assigned_by_user_id: String,
    assigned_at: String,
    toy_ids: Vec<String>,
    checkin_schedule: Vec<ScheduleSlotResponse>,
}

fn session_response(
    conn: &rusqlite::Connection,
    s: play_sessions::PlaySession,
) -> Result<SessionResponse, ApiError> {
    let toy_ids = play_sessions::toy_ids_for_session(conn, &s.id).map_err(|_| INTERNAL_ERROR)?;
    let checkin_schedule = play_sessions::schedule_for_session(conn, &s.id)
        .map_err(|_| INTERNAL_ERROR)?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(SessionResponse {
        id: s.id,
        link_id: s.link_id,
        template_id: s.template_id,
        title: s.title,
        setup_notes: s.setup_notes,
        status: s.status,
        planned_duration_seconds: s.planned_duration_seconds,
        checkin_template_id: s.checkin_template_id,
        checkin_interval_seconds: s.checkin_interval_seconds,
        started_at: s.started_at.map(iso8601),
        ended_at: s.ended_at.map(iso8601),
        safety_check_ok: s.safety_check_ok,
        judgement_notes: s.judgement_notes,
        reward_assignment_id: s.reward_assignment_id,
        punishment_assignment_id: s.punishment_assignment_id,
        assigned_by_user_id: s.assigned_by_user_id,
        assigned_at: iso8601(s.assigned_at),
        toy_ids,
        checkin_schedule,
    })
}

/// Ownership check shared by every `.../play-sessions/{id}` route:
/// resolves the session's link parties and confirms the caller is one
/// of them. 404, not 403, on mismatch (same posture as the rest of
/// this API). Returns `(keyholder_id, submissive_id)`.
fn require_reachable_session(
    conn: &rusqlite::Connection,
    user: &CurrentUser,
    session: &play_sessions::PlaySession,
) -> Result<(String, String), ApiError> {
    let (keyholder_id, submissive_id) = links::parties(conn, &session.link_id)
        .map_err(|_| INTERNAL_ERROR)?
        .ok_or(INTERNAL_ERROR)?;
    match user.role {
        Role::Keyholder => {
            if keyholder_id != user.user_id {
                return Err(NOT_FOUND);
            }
        }
        Role::Submissive => {
            if submissive_id != user.user_id {
                return Err(NOT_FOUND);
            }
        }
    }
    Ok((keyholder_id, submissive_id))
}

#[derive(Deserialize)]
struct CreateSessionRequest {
    template_id: Option<String>,
    title: Option<String>,
    setup_notes: Option<String>,
    #[serde(default)]
    toy_ids: Vec<String>,
    planned_duration_seconds: Option<i64>,
    checkin_template_id: Option<String>,
    checkin_interval_seconds: Option<i64>,
    started_at: Option<String>,
    ended_at: Option<String>,
}

/// `POST /keyholder/submissives/{id}/play-sessions`.
async fn create_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<SessionResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    let started_at = match req.started_at.as_deref() {
        Some(s) => Some(parse_iso8601(s).ok_or(BAD_REQUEST)?),
        None => None,
    };
    let ended_at = match req.ended_at.as_deref() {
        Some(s) => Some(parse_iso8601(s).ok_or(BAD_REQUEST)?),
        None => None,
    };
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let mut conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        let session = play_sessions::create(
            &mut conn,
            play_sessions::NewSession {
                link_id: &link_id,
                submissive_id: &submissive_id,
                template_id: req.template_id.as_deref(),
                title: req.title.as_deref(),
                setup_notes: req.setup_notes.as_deref(),
                toy_ids: &req.toy_ids,
                planned_duration_seconds: req.planned_duration_seconds,
                checkin_template_id: req.checkin_template_id.as_deref(),
                checkin_interval_seconds: req.checkin_interval_seconds,
                started_at,
                ended_at,
                assigned_by_user_id: &user.user_id,
            },
        )
        .map_err(|e| match e {
            play_sessions::CreateError::TemplateNotFound => NOT_FOUND,
            play_sessions::CreateError::MissingTitle => BAD_REQUEST,
            play_sessions::CreateError::InvalidToy => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_toy",
                "a toy does not belong to this submissive",
            ),
            play_sessions::CreateError::Db(_) => INTERNAL_ERROR,
        })?;
        session_response(&conn, session).map(Json)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
}

async fn list_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<SessionResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        let list = play_sessions::list_for_link(&conn, &link_id, q.status.as_deref())
            .map_err(|_| INTERNAL_ERROR)?;
        let mut out = Vec::with_capacity(list.len());
        for s in list {
            out.push(session_response(&conn, s)?);
        }
        Ok(Json(out))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn list_own(
    State(pool): State<Pool>,
    user: CurrentUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<SessionResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let list = play_sessions::list_for_link(&conn, &link_id, q.status.as_deref())
            .map_err(|_| INTERNAL_ERROR)?;
        let mut out = Vec::with_capacity(list.len());
        for s in list {
            out.push(session_response(&conn, s)?);
        }
        Ok(Json(out))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn get_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SessionResponse>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let session = play_sessions::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_session(&conn, &user, &session)?;
        session_response(&conn, session).map(Json)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `POST .../play-sessions/{id}/start` — either role, own link.
async fn start_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SessionResponse>, ApiError> {
    let pool2 = pool.clone();
    let id2 = id.clone();
    let acting_user_id = user.user_id.clone();
    let (session, keyholder_id, submissive_id) =
        tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
            let conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
            let current = play_sessions::get(&conn, &id2)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
            let (keyholder_id, submissive_id) = require_reachable_session(&conn, &user, &current)?;
            let updated = play_sessions::start(&conn, &id2).map_err(|e| match e {
                play_sessions::StartError::NotFound => NOT_FOUND,
                play_sessions::StartError::Conflict => CONFLICT,
                play_sessions::StartError::Db(_) => INTERNAL_ERROR,
            })?;
            Ok((updated, keyholder_id, submissive_id))
        })
        .await
        .map_err(|_| INTERNAL_ERROR)??;

    let other = if acting_user_id == keyholder_id {
        &submissive_id
    } else {
        &keyholder_id
    };
    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: other,
            link_id: Some(&session.link_id),
            notification_type: "play_session.started",
            title: &format!("Play session started: {}", session.title),
            body: None,
            link_path: Some(&format!("/play-sessions/{}", session.id)),
            related_entity_type: Some("play_sessions"),
            related_entity_id: Some(&session.id),
            push: true,
        },
    )
    .await;

    let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
    session_response(&conn, session).map(Json)
}

#[derive(Deserialize, Default)]
struct EndSessionRequest {
    safety_check_ok: Option<bool>,
}

/// `POST .../play-sessions/{id}/end` — either role, own link.
async fn end_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<EndSessionRequest>,
) -> Result<Json<SessionResponse>, ApiError> {
    let pool2 = pool.clone();
    let id2 = id.clone();
    let (session, keyholder_id) = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
        let current = play_sessions::get(&conn, &id2)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let (keyholder_id, _) = require_reachable_session(&conn, &user, &current)?;
        let updated =
            play_sessions::end(&conn, &id2, req.safety_check_ok).map_err(|e| match e {
                play_sessions::EndError::NotFound => NOT_FOUND,
                play_sessions::EndError::Conflict => CONFLICT,
                play_sessions::EndError::Db(_) => INTERNAL_ERROR,
            })?;
        Ok((updated, keyholder_id))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: &keyholder_id,
            link_id: Some(&session.link_id),
            notification_type: "play_session.pending_judgement",
            title: &format!("Play session ended, awaiting judgement: {}", session.title),
            body: None,
            link_path: Some(&format!("/keyholder/play-sessions/{}", session.id)),
            related_entity_type: Some("play_sessions"),
            related_entity_id: Some(&session.id),
            push: true,
        },
    )
    .await;

    let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
    session_response(&conn, session).map(Json)
}

#[derive(Deserialize)]
struct JudgementConsequenceRequest {
    template_id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    effect_kind: Option<String>,
    time_extension_seconds: Option<i64>,
    time_reduction_seconds: Option<i64>,
    points_delta: Option<i64>,
}

#[derive(Deserialize, Default)]
struct JudgementRequest {
    judgement_notes: Option<String>,
    reward: Option<JudgementConsequenceRequest>,
    punishment: Option<JudgementConsequenceRequest>,
}

fn to_consequence(r: &JudgementConsequenceRequest) -> play_sessions::JudgementConsequence<'_> {
    play_sessions::JudgementConsequence {
        template_id: r.template_id.as_deref(),
        title: r.title.as_deref(),
        description: r.description.as_deref(),
        effect_kind: r.effect_kind.as_deref(),
        time_extension_seconds: r.time_extension_seconds,
        time_reduction_seconds: r.time_reduction_seconds,
        points_delta: r.points_delta,
    }
}

/// `PATCH /keyholder/play-sessions/{id}/judgement` — callable multiple
/// times before `complete` (e.g. notes now, judgement later); the
/// `play_session.judged` notification (09-notifications.md §3) fires
/// on `complete`, not here, since judgement can be revised more than
/// once and completion is the actual finalization point.
async fn judge_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<JudgementRequest>,
) -> Result<Json<SessionResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:assignments")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<Json<SessionResponse>, ApiError> {
        let mut conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let current = play_sessions::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let (_, submissive_id) = require_reachable_session(&conn, &user, &current)?;

        let reward = req.reward.as_ref().map(to_consequence);
        let punishment = req.punishment.as_ref().map(to_consequence);

        let updated = play_sessions::judge(
            &mut conn,
            &id,
            &submissive_id,
            &user.user_id,
            play_sessions::Judgement {
                judgement_notes: req.judgement_notes.as_deref(),
                reward,
                punishment,
            },
        )
        .map_err(|e| match e {
            play_sessions::JudgementError::NotFound => NOT_FOUND,
            play_sessions::JudgementError::AlreadyCompleted => CONFLICT,
            play_sessions::JudgementError::Assignment(_) => INTERNAL_ERROR,
            play_sessions::JudgementError::Db(_) => INTERNAL_ERROR,
        })?;
        session_response(&conn, updated).map(Json)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `PATCH /keyholder/play-sessions/{id}/complete` — judgement is
/// optional; moves `pending_judgement` -> `completed`. This is where
/// `play_session.judged` (09-notifications.md §3) fires, since
/// judgement itself can be revised multiple times before this.
async fn complete_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SessionResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let id2 = id.clone();
    let (session, submissive_id) = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
        let current = play_sessions::get(&conn, &id2)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let (_, submissive_id) = require_reachable_session(&conn, &user, &current)?;
        let updated = play_sessions::complete(&conn, &id2).map_err(|e| match e {
            play_sessions::CompleteError::NotFound => NOT_FOUND,
            play_sessions::CompleteError::Conflict => CONFLICT,
            play_sessions::CompleteError::Db(_) => INTERNAL_ERROR,
        })?;
        Ok((updated, submissive_id))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: &submissive_id,
            link_id: Some(&session.link_id),
            notification_type: "play_session.judged",
            title: &format!("Play session judged: {}", session.title),
            body: session.judgement_notes.as_deref(),
            link_path: Some(&format!("/submissive/play-sessions/{}", session.id)),
            related_entity_type: Some("play_sessions"),
            related_entity_id: Some(&session.id),
            push: true,
        },
    )
    .await;

    let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
    session_response(&conn, session).map(Json)
}

/// `PATCH /keyholder/play-sessions/{id}/cancel` — from `scheduled` or
/// `in_progress` only; no judgement applies, no notification fires
/// (not in the trigger matrix).
async fn cancel_session(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SessionResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<Json<SessionResponse>, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let current = play_sessions::get(&conn, &id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        require_reachable_session(&conn, &user, &current)?;
        let updated = play_sessions::cancel(&conn, &id).map_err(|e| match e {
            play_sessions::CancelError::NotFound => NOT_FOUND,
            play_sessions::CancelError::Conflict => CONFLICT,
            play_sessions::CancelError::Db(_) => INTERNAL_ERROR,
        })?;
        session_response(&conn, updated).map(Json)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/keyholder/play-session-templates",
            get(list_templates).post(create_template),
        )
        .route(
            "/keyholder/play-session-templates/{id}",
            patch(patch_template),
        )
        .route(
            "/submissive/play-session-templates",
            get(list_templates_for_submissive),
        )
        .route(
            "/keyholder/submissives/{id}/play-sessions",
            post(create_session).get(list_for_keyholder),
        )
        .route("/submissive/play-sessions", get(list_own))
        .route("/keyholder/play-sessions/{id}", get(get_session))
        .route("/submissive/play-sessions/{id}", get(get_session))
        .route("/keyholder/play-sessions/{id}/start", post(start_session))
        .route("/submissive/play-sessions/{id}/start", post(start_session))
        .route("/keyholder/play-sessions/{id}/end", post(end_session))
        .route("/submissive/play-sessions/{id}/end", post(end_session))
        .route(
            "/keyholder/play-sessions/{id}/judgement",
            patch(judge_session),
        )
        .route(
            "/keyholder/play-sessions/{id}/complete",
            patch(complete_session),
        )
        .route(
            "/keyholder/play-sessions/{id}/cancel",
            patch(cancel_session),
        )
}
