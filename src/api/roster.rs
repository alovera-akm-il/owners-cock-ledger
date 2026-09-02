//! `GET /keyholder/submissives` (03-api-design.md §2). The documented
//! per-row summary card (current lock state, last verification outcome,
//! pending items count) needs Phase 2/3 domains that don't exist yet —
//! this returns the identity/link fields Phase 1 actually has and leaves
//! the rest for those phases to add, rather than fabricating placeholder
//! values.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR};
use crate::auth::session::{CurrentUser, Role};
use crate::db;
use crate::db::Pool;
use crate::domain::links;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such link");
const INVALID_TRANSITION: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "invalid_transition",
    "that status change isn't allowed from the link's current status",
);

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
    user.require_scope("read:submissives")
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

/// Resolves `submissive_id` to the caller's own link id, 404ing rather
/// than 403ing so the response can't confirm another Keyholder's
/// submissive exists (02-roles-and-permissions.md §1 principle 2).
fn resolve_link_id(
    conn: &rusqlite::Connection,
    keyholder_id: &str,
    submissive_id: &str,
) -> Result<String, ApiError> {
    links::active_or_paused_link_for_keyholder(conn, keyholder_id, submissive_id)
        .map_err(|_| INTERNAL_ERROR)?
        .ok_or(NOT_FOUND)
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LinkStatusTarget {
    Paused,
    Ended,
}

#[derive(Deserialize)]
struct PatchLinkRequest {
    status: LinkStatusTarget,
}

/// `PATCH /keyholder/submissives/{id}/link` (03-api-design.md §2) —
/// only forward transitions; there's no way back to `active` here.
async fn patch_link(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<PatchLinkRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:invites")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = resolve_link_id(&conn, &user.user_id, &submissive_id)?;
        let new_status = match req.status {
            LinkStatusTarget::Paused => "paused",
            LinkStatusTarget::Ended => "ended",
        };
        match links::set_status(&conn, &link_id, &user.user_id, new_status) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(links::SetStatusError::NotFound) => Err(NOT_FOUND),
            Err(links::SetStatusError::InvalidTransition) => Err(INVALID_TRANSITION),
            Err(links::SetStatusError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct PatchLinkSettingsRequest {
    self_report_allowed: bool,
    catalog_visible_to_submissive: bool,
    #[serde(default)]
    points_enabled: bool,
}

/// `PATCH /keyholder/submissives/{id}/link/settings` (03-api-design.md
/// §2).
async fn patch_link_settings(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<PatchLinkSettingsRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:invites")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = resolve_link_id(&conn, &user.user_id, &submissive_id)?;
        let updated = links::set_settings(
            &conn,
            &link_id,
            &user.user_id,
            links::LinkSettings {
                self_report_allowed: req.self_report_allowed,
                catalog_visible_to_submissive: req.catalog_visible_to_submissive,
                points_enabled: req.points_enabled,
            },
        )
        .map_err(|_| INTERNAL_ERROR)?;
        if updated {
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(NOT_FOUND)
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/keyholder/submissives", get(roster))
        .route("/keyholder/submissives/{id}/link", patch(patch_link))
        .route(
            "/keyholder/submissives/{id}/link/settings",
            patch(patch_link_settings),
        )
}
