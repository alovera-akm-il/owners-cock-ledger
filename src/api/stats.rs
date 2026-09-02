//! `GET /submissive/stats`, `GET /keyholder/submissives/{id}/stats`
//! (03-api-design.md §15) — a read-only reporting layer over data this
//! design already captures elsewhere; see `domain::stats` for the
//! actual aggregation queries.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, Pool};
use crate::domain::{links, stats};

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const BAD_REQUEST: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "bad_request",
    "period must be one of all, 30d, 90d, 365d",
);

#[derive(Deserialize)]
struct StatsQuery {
    #[serde(default = "default_period")]
    period: String,
}

fn default_period() -> String {
    "all".to_string()
}

#[derive(Serialize)]
struct SessionLengthsResponse {
    shortest_seconds: i64,
    longest_seconds: i64,
    average_seconds: i64,
}

#[derive(Serialize)]
struct VerificationCountsResponse {
    verified: i64,
    failed: i64,
    missed: i64,
}

#[derive(Serialize)]
struct TaskCountsResponse {
    assigned: i64,
    completed: i64,
    failed: i64,
    escalated: i64,
}

#[derive(Serialize)]
struct TimerAdjustmentsResponse {
    added_seconds: i64,
    removed_seconds: i64,
}

#[derive(Serialize)]
struct StatsResponse {
    period: String,
    current_streak_seconds: i64,
    personal_best_streak_seconds: i64,
    consistency_pct: i64,
    session_lengths: SessionLengthsResponse,
    verification: VerificationCountsResponse,
    tasks: TaskCountsResponse,
    rewards_given: i64,
    punishments_given: i64,
    timer_adjustments: TimerAdjustmentsResponse,
    lifetime_locked_seconds: i64,
}

impl From<stats::Stats> for StatsResponse {
    fn from(s: stats::Stats) -> Self {
        Self {
            period: s.period,
            current_streak_seconds: s.current_streak_seconds,
            personal_best_streak_seconds: s.personal_best_streak_seconds,
            consistency_pct: s.consistency_pct,
            session_lengths: SessionLengthsResponse {
                shortest_seconds: s.session_lengths.shortest_seconds,
                longest_seconds: s.session_lengths.longest_seconds,
                average_seconds: s.session_lengths.average_seconds,
            },
            verification: VerificationCountsResponse {
                verified: s.verification.verified,
                failed: s.verification.failed,
                missed: s.verification.missed,
            },
            tasks: TaskCountsResponse {
                assigned: s.tasks.assigned,
                completed: s.tasks.completed,
                failed: s.tasks.failed,
                escalated: s.tasks.escalated,
            },
            rewards_given: s.rewards_given,
            punishments_given: s.punishments_given,
            timer_adjustments: TimerAdjustmentsResponse {
                added_seconds: s.timer_adjustments.added_seconds,
                removed_seconds: s.timer_adjustments.removed_seconds,
            },
            lifetime_locked_seconds: s.lifetime_locked_seconds,
        }
    }
}

async fn own_stats(
    State(pool): State<Pool>,
    user: CurrentUser,
    Query(q): Query<StatsQuery>,
) -> Result<Json<StatsResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    if !stats::valid_period(&q.period) {
        return Err(BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_or_paused_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let result = stats::compute(&conn, &link_id, &user.user_id, &q.period)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(result.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn submissive_stats_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<StatsResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    if !stats::valid_period(&q.period) {
        return Err(BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        let result = stats::compute(&conn, &link_id, &submissive_id, &q.period)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(result.into()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/submissive/stats", get(own_stats))
        .route(
            "/keyholder/submissives/{id}/stats",
            get(submissive_stats_for_keyholder),
        )
}
