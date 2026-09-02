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

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db;
use crate::db::Pool;
use crate::domain::links;
use crate::notify;

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

#[derive(Serialize)]
struct EndRequestResponse {
    submissive_id: String,
    submissive_display_name: String,
    requested_at: String,
    reason: Option<String>,
    escalated_at: Option<String>,
}

impl From<links::PendingEndRequest> for EndRequestResponse {
    fn from(r: links::PendingEndRequest) -> Self {
        Self {
            submissive_id: r.submissive_id,
            submissive_display_name: r.submissive_display_name,
            requested_at: iso8601(r.requested_at),
            reason: r.reason,
            escalated_at: r.escalated_at.map(iso8601),
        }
    }
}

/// `GET /keyholder/link-end-requests` (06-future-extensions.md §2) —
/// every pending request across this Keyholder's whole roster; used
/// both for the per-submissive decline UI and for the
/// impossible-to-miss banner once one escalates (client-side, via
/// `notifications.js`, so it appears on every page without every
/// template needing to know about it).
async fn list_end_requests(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<EndRequestResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let list = links::pending_end_requests_for_keyholder(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `GET /submissive/link/end-request` — the submissive's own pending
/// request, if any, so their account page can render its current
/// state on load. `Null` (not a `404`) when nothing's pending — this
/// is a status check, not a lookup of a specific resource.
async fn own_end_request(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Option<EndRequestResponse>>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let req =
            links::own_pending_end_request(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(req.map(Into::into)))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize, Default)]
struct RequestEndRequest {
    reason: Option<String>,
}

const CONFLICT: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "conflict",
    "request conflicts with current state",
);

/// `POST /submissive/link/end-request` (06-future-extensions.md §2) —
/// a request, not an action: doesn't change link status or anything
/// else operative about the relationship. `409` if one's already
/// pending or there's no active/paused link to request against.
async fn request_end(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<RequestEndRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let link_id = tokio::task::spawn_blocking(move || -> Result<String, ApiError> {
        let conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
        links::request_end(&conn, &user.user_id, req.reason.as_deref()).map_err(|e| match e {
            links::RequestEndError::NoLink => NOT_FOUND,
            links::RequestEndError::AlreadyPending => CONFLICT,
            links::RequestEndError::Db(_) => INTERNAL_ERROR,
        })
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
    if let Some((keyholder_id, _)) = links::parties(&conn, &link_id).map_err(|_| INTERNAL_ERROR)? {
        let _ = notify::notify(
            &pool,
            notify::Event {
                user_id: &keyholder_id,
                link_id: Some(&link_id),
                notification_type: "link.end_requested",
                title: "Your submissive has requested to end the link",
                body: None,
                link_path: None,
                related_entity_type: Some("keyholder_submissive_links"),
                related_entity_id: Some(&link_id),
                push: true,
            },
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /submissive/link/end-request` — withdraws it at any time,
/// no confirmation needed. A no-op (still `204`) if there was nothing
/// pending.
async fn withdraw_end_request(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        links::withdraw_end_request(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize, Default)]
struct DeclineEndRequestRequest {
    response_note: Option<String>,
}

/// `POST /keyholder/submissives/{id}/link/end-request/decline` —
/// clears the request without ending the link; the optional note is
/// delivered back to the submissive so declining isn't
/// indistinguishable from being ignored.
async fn decline_end_request(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<DeclineEndRequestRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:invites")
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let (link_id, target_submissive_id) =
        tokio::task::spawn_blocking(move || -> Result<(String, String), ApiError> {
            let conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
            let link_id = resolve_link_id(&conn, &user.user_id, &submissive_id)?;
            let target_submissive_id = links::decline_end_request(&conn, &link_id, &user.user_id)
                .map_err(|e| match e {
                links::DeclineEndRequestError::NotFound => NOT_FOUND,
                links::DeclineEndRequestError::Db(_) => INTERNAL_ERROR,
            })?;
            Ok((link_id, target_submissive_id))
        })
        .await
        .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: &target_submissive_id,
            link_id: Some(&link_id),
            notification_type: "link.end_request_declined",
            title: "Your Keyholder declined the end request",
            body: req.response_note.as_deref(),
            link_path: None,
            related_entity_type: Some("keyholder_submissive_links"),
            related_entity_id: Some(&link_id),
            push: true,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/keyholder/submissives", get(roster))
        .route("/keyholder/submissives/{id}/link", patch(patch_link))
        .route(
            "/keyholder/submissives/{id}/link/settings",
            patch(patch_link_settings),
        )
        .route("/keyholder/link-end-requests", get(list_end_requests))
        .route(
            "/submissive/link/end-request",
            get(own_end_request)
                .post(request_end)
                .delete(withdraw_end_request),
        )
        .route(
            "/keyholder/submissives/{id}/link/end-request/decline",
            axum::routing::post(decline_end_request),
        )
}
