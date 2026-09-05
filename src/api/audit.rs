//! `GET /keyholder/audit-log` — a Keyholder's own read-only view over
//! `audit_log` (`docs/16-mockup-implementation-gaps.md` item 13). Scoped
//! to whatever `domain::audit::list_for_keyholder` already resolves as
//! relevant to this Keyholder; this layer only adds human-facing labels
//! (actor "You"/submissive name/System/Admin (CLI), a title per action,
//! and a plain-text rendering of the one action that carries a real
//! `detail` payload) on top of that.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, Pool};
use crate::domain::audit;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");

#[derive(Serialize)]
struct AuditRowResponse {
    id: String,
    occurred_at: String,
    action: String,
    action_title: &'static str,
    submissive_id: Option<String>,
    submissive_display_name: Option<String>,
    /// "you" | "submissive" | "system" | "admin_cli" — which of those
    /// four the row's actor is, so the UI can style/label it without
    /// re-deriving the same logic client-side.
    actor_kind: &'static str,
    actor_display_name: Option<String>,
    detail_text: Option<String>,
}

/// A title per the 13 action strings actually written today
/// (confirmed against every `audit::record` call site) — deliberately
/// exhaustive rather than a fallback-to-raw-string default, so a
/// future new action is a compile-time reminder to add a title here
/// too, not a silently ugly row.
fn action_title(action: &str) -> &'static str {
    match action {
        "toy.retired" => "Retired toy",
        "toy.removal_declined" => "Declined toy removal request",
        "safety_alert.raised" => "Safety alert raised",
        "link.end_request_declined" => "Declined link-end request",
        "link.oversight_resumed" => "Resumed oversight pause",
        "invite.redeemed" => "Invite redeemed",
        "assignment.failed" => "Marked task failed",
        "assignment.auto_failed" => "Task auto-failed (deadline passed)",
        "user.created_via_admin_cli" => "Account created (admin CLI)",
        "user.password_reset_issued_via_admin_cli" => "Password reset issued (admin CLI)",
        "user.two_factor_disabled_via_admin_cli" => "Two-factor disabled (admin CLI)",
        "user.unlocked_via_admin_cli" => "Account lockout cleared (admin CLI)",
        "link.force_ended_via_admin_cli" => "Link force-ended (admin CLI)",
        _ => "Unrecognized action",
    }
}

/// The one action whose `detail` is a real payload rather than just
/// the system/admin-CLI actor-type tag `record()` auto-injects — see
/// `domain::links::resume_oversight`.
fn detail_text(action: &str, detail: Option<&str>) -> Option<String> {
    if action != "link.oversight_resumed" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(detail?).ok()?;
    let elapsed = value.get("elapsed_seconds")?.as_i64()?;
    let shifted = value.get("shifted_assignment_count")?.as_i64()?;
    let hours = elapsed / 3600;
    let minutes = (elapsed % 3600) / 60;
    Some(format!(
        "Paused {hours}h {minutes}m; {shifted} deadline(s) shifted forward by that much"
    ))
}

fn row_response(keyholder_id: &str, row: audit::LogRow) -> AuditRowResponse {
    let (actor_kind, actor_display_name) = match &row.actor_user_id {
        Some(id) if id == keyholder_id => ("you", Some("You".to_string())),
        Some(_) => ("submissive", row.actor_display_name.clone()),
        None => {
            let is_system = row
                .detail
                .as_deref()
                .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
                .and_then(|v| v.get("actor_type").and_then(|t| t.as_str().map(String::from)))
                == Some("system".to_string());
            if is_system {
                ("system", Some("System".to_string()))
            } else {
                ("admin_cli", Some("Admin (CLI)".to_string()))
            }
        }
    };
    AuditRowResponse {
        action_title: action_title(&row.action),
        detail_text: detail_text(&row.action, row.detail.as_deref()),
        id: row.id,
        occurred_at: iso8601(row.occurred_at),
        action: row.action,
        submissive_id: row.submissive_id,
        submissive_display_name: row.submissive_display_name,
        actor_kind,
        actor_display_name,
    }
}

async fn list(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<AuditRowResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let rows = audit::list_for_keyholder(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(
            rows.into_iter()
                .map(|r| row_response(&user.user_id, r))
                .collect(),
        ))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new().route("/keyholder/audit-log", get(list))
}
