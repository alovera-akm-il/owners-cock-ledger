//! Safety alerts (03-api-design.md §5) — the always-available escape
//! hatch, orthogonal to the normal review flow. A submissive can raise
//! one regardless of any other state; a Keyholder sees them across every
//! active link, unresolved ones surfaced first.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, Pool};
use crate::domain::{links, safety};
use crate::notify;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const NO_ACTIVE_LINK: ApiError = ApiError::new(
    StatusCode::NOT_FOUND,
    "no_active_link",
    "no active Keyholder link to raise this against",
);

#[derive(Deserialize, Default)]
pub struct RaiseSafetyAlertRequest {
    message: Option<String>,
}

/// `POST /submissive/safety-alert` — intentionally minimal payload, so
/// it's fast to fire under exactly the circumstances someone doesn't
/// want to be filling out a form.
async fn raise_safety_alert(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<RaiseSafetyAlertRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let (alert_id, link_id, keyholder_id) =
        tokio::task::spawn_blocking(move || -> Result<(String, String, String), ApiError> {
            let mut conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
            let link_id = links::active_link_for_submissive(&conn, &user.user_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NO_ACTIVE_LINK)?;
            let alert_id = safety::raise(
                &mut conn,
                safety::Raise {
                    submissive_id: &user.user_id,
                    link_id: &link_id,
                    raised_via: safety::RaisedVia::Submissive,
                    related_checkin_id: None,
                    message: req.message.as_deref(),
                },
            )
            .map_err(|_| INTERNAL_ERROR)?;
            let (keyholder_id, _) = links::parties(&conn, &link_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
            Ok((alert_id, link_id, keyholder_id))
        })
        .await
        .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: &keyholder_id,
            link_id: Some(&link_id),
            notification_type: "safety.alert_raised",
            title: "Safety alert raised",
            body: Some("Check on your submissive now."),
            link_path: Some("/keyholder/safety-alerts"),
            related_entity_type: Some("safety_alerts"),
            related_entity_id: Some(&alert_id),
            push: true,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct AlertResponse {
    id: String,
    submissive_id: String,
    raised_at: String,
    raised_via: String,
    message: Option<String>,
    acknowledged_at: Option<String>,
    acknowledged_by_user_id: Option<String>,
    resolved_at: Option<String>,
}

impl From<safety::Alert> for AlertResponse {
    fn from(a: safety::Alert) -> Self {
        Self {
            id: a.id,
            submissive_id: a.submissive_id,
            raised_at: iso8601(a.raised_at),
            raised_via: a.raised_via,
            message: a.message,
            acknowledged_at: a.acknowledged_at.map(iso8601),
            acknowledged_by_user_id: a.acknowledged_by_user_id,
            resolved_at: a.resolved_at.map(iso8601),
        }
    }
}

/// `GET /keyholder/safety-alerts` — across every active link, unresolved
/// first.
async fn list_safety_alerts(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<AlertResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:safety-alerts")
        .map_err(|_| FORBIDDEN)?;
    let alerts = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<safety::Alert>> {
        let conn = pool.get()?;
        let link_ids = links::active_link_ids_for_keyholder(&conn, &user.user_id)?;
        Ok(safety::list_for_links(&conn, &link_ids)?)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(alerts.into_iter().map(Into::into).collect()))
}

fn keyholder_owns_alert(
    conn: &rusqlite::Connection,
    keyholder_id: &str,
    link_id: &str,
) -> Result<(), ApiError> {
    let owns: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM keyholder_submissive_links WHERE id = ?1 AND keyholder_id = ?2",
            rusqlite::params![link_id, keyholder_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| INTERNAL_ERROR)?;
    owns.map(|_| ()).ok_or(NOT_FOUND)
}

#[derive(Deserialize)]
struct PatchAlertRequest {
    #[serde(default)]
    acknowledged: bool,
    #[serde(default)]
    resolved: bool,
}

/// `PATCH /keyholder/safety-alerts/{id}` — `{acknowledged: true}` and/or
/// `{resolved: true}`; either or both in one call.
async fn patch_safety_alert(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<PatchAlertRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:safety-alerts")
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let id2 = id.clone();
    let newly_acknowledged =
        tokio::task::spawn_blocking(move || -> Result<Option<String>, ApiError> {
            let conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
            let alert = safety::get(&conn, &id2)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
            keyholder_owns_alert(&conn, &user.user_id, &alert.link_id)?;

            let mut newly_acknowledged = None;
            if req.acknowledged {
                if alert.acknowledged_at.is_none() {
                    newly_acknowledged = Some(alert.submissive_id.clone());
                }
                safety::acknowledge(&conn, &id2, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
            }
            if req.resolved {
                safety::resolve(&conn, &id2).map_err(|_| INTERNAL_ERROR)?;
            }
            Ok(newly_acknowledged)
        })
        .await
        .map_err(|_| INTERNAL_ERROR)??;

    if let Some(submissive_id) = newly_acknowledged {
        let _ = notify::notify(
            &pool,
            notify::Event {
                user_id: &submissive_id,
                link_id: None,
                notification_type: "safety.acknowledged",
                title: "Your Keyholder saw your safety alert",
                body: None,
                link_path: None,
                related_entity_type: Some("safety_alerts"),
                related_entity_id: Some(&id),
                push: true,
            },
        )
        .await;
    }

    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/submissive/safety-alert", post(raise_safety_alert))
        .route("/keyholder/safety-alerts", get(list_safety_alerts))
        .route("/keyholder/safety-alerts/{id}", patch(patch_safety_alert))
}
