//! `GET /keyholder/submissives` (03-api-design.md §2). The documented
//! per-row summary card (current lock state, last verification outcome,
//! pending items count) needs Phase 2/3 domains that don't exist yet —
//! this returns the identity/link fields Phase 1 actually has and leaves
//! the rest for those phases to add, rather than fabricating placeholder
//! values.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR};
use crate::auth::session::{CurrentUser, Role};
use crate::db;
use crate::db::Pool;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");

#[derive(Deserialize)]
pub struct RosterQuery {
    status: Option<String>,
}

#[derive(Serialize)]
pub struct RosterEntry {
    submissive_id: String,
    display_name: String,
    link_id: String,
    status: String,
    started_at: i64,
}

async fn roster(
    State(pool): State<Pool>,
    user: CurrentUser,
    Query(query): Query<RosterQuery>,
) -> Result<Json<Vec<RosterEntry>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    let status = query.status.unwrap_or_else(|| "active".to_string());

    let rows = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<RosterEntry>> {
        let conn = pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT u.id, u.display_name, l.id, l.status, l.started_at
             FROM keyholder_submissive_links l
             JOIN users u ON u.id = l.submissive_id
             WHERE l.keyholder_id = ?1 AND l.status = ?2
             ORDER BY l.started_at DESC",
        )?;
        let rows = stmt
            .query_map(params![user.user_id, status], |row| {
                Ok(RosterEntry {
                    submissive_id: row.get(0)?,
                    display_name: row.get(1)?,
                    link_id: row.get(2)?,
                    status: row.get(3)?,
                    started_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(rows))
}

pub fn router() -> Router<db::AppState> {
    Router::new().route("/keyholder/submissives", get(roster))
}
