//! Server-rendered pages (07-tech-stack.md §3): askama templates,
//! progressively enhanced with jQuery calling the `/api/v1` JSON routes
//! built in `api/`. Thin over the same domain layer the API uses — page
//! handlers read data to render, they don't duplicate business logic.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use rusqlite::params;

use crate::auth::session::{self, CurrentUser, Role, SESSION_COOKIE_NAME};
use crate::db;
use crate::db::Pool;
use crate::domain::chastity::{confinement, devices};
use crate::domain::rewards_punishments::{assignments, templates};
use crate::domain::verification::{codes, policy};
use crate::domain::{links, proofs};

fn render<T: Template>(tpl: T) -> Response {
    match tpl.render() {
        Ok(body) => Html(body).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Resolves the session cookie to a `CurrentUser`, or `None` — unlike the
/// `CurrentUser` axum extractor (which rejects with `401` for the JSON
/// API), page handlers need a non-erroring lookup so they can redirect to
/// `/login` instead of showing a bare status code in a browser tab.
async fn resolve_current_user(pool: &Pool, jar: &CookieJar) -> Option<CurrentUser> {
    let session_id = jar.get(SESSION_COOKIE_NAME)?.value().to_string();
    let pool = pool.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get().ok()?;
        session::resolve(&conn, &session_id).ok().flatten()
    })
    .await
    .ok()
    .flatten()
}

fn initial_of(display_name: &str) -> String {
    display_name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn fmt_duration(seconds: i64) -> String {
    let sign = if seconds < 0 { "-" } else { "" };
    let seconds = seconds.abs();
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3600;
    let minutes = (seconds % 3600) / 60;
    if days > 0 {
        format!("{sign}{days}d {hours}h")
    } else if hours > 0 {
        format!("{sign}{hours}h {minutes}m")
    } else {
        format!("{sign}{minutes}m")
    }
}

async fn index(State(pool): State<Pool>, jar: CookieJar) -> Redirect {
    match resolve_current_user(&pool, &jar).await {
        Some(user) if user.role == Role::Keyholder => Redirect::to("/dashboard"),
        Some(_) => Redirect::to("/submissive"),
        None => Redirect::to("/login"),
    }
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate;

async fn login_page() -> Response {
    render(LoginTemplate)
}

#[derive(Template)]
#[template(path = "redeem_invite.html")]
struct RedeemInviteTemplate;

async fn redeem_invite_page() -> Response {
    render(RedeemInviteTemplate)
}

struct RosterRow {
    submissive_id: String,
    display_name: String,
    initial: String,
    linked_days: i64,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    display_name: String,
    initial: String,
    roster: Vec<RosterRow>,
    roster_is_empty: bool,
}

async fn dashboard_page(State(pool): State<Pool>, jar: CookieJar) -> Response {
    let Some(user) = resolve_current_user(&pool, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    if user.role != Role::Keyholder {
        return Redirect::to("/submissive").into_response();
    }

    let keyholder_id = user.user_id.clone();
    let roster = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<RosterRow>> {
        let conn = pool.get()?;
        let now = session::now();
        let mut stmt = conn.prepare(
            "SELECT u.id, u.display_name, l.started_at
             FROM keyholder_submissive_links l
             JOIN users u ON u.id = l.submissive_id
             WHERE l.keyholder_id = ?1 AND l.status = 'active'
             ORDER BY l.started_at DESC",
        )?;
        let rows = stmt
            .query_map(params![keyholder_id], |row| {
                let submissive_id: String = row.get(0)?;
                let display_name: String = row.get(1)?;
                let started_at: i64 = row.get(2)?;
                Ok(RosterRow {
                    submissive_id,
                    initial: initial_of(&display_name),
                    display_name,
                    linked_days: (now - started_at) / 86_400,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await;

    let roster = match roster {
        Ok(Ok(rows)) => rows,
        _ => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    render(DashboardTemplate {
        initial: initial_of(&user.display_name),
        display_name: user.display_name,
        roster_is_empty: roster.is_empty(),
        roster,
    })
}

struct RecentSubmission {
    status: String,
    kind: String,
    submitted_ago: String,
}

#[derive(Template)]
#[template(path = "submissive_dashboard.html")]
struct SubmissiveDashboardTemplate {
    display_name: String,
    initial: String,
    locked: bool,
    time_remaining_text: Option<String>,
    overdue: bool,
    clock_paused: bool,
    clock_pause_message: Option<String>,
    current_code: Option<String>,
    current_code_expires_text: Option<String>,
    recent: Vec<RecentSubmission>,
}

async fn submissive_dashboard_page(State(pool): State<Pool>, jar: CookieJar) -> Response {
    let Some(user) = resolve_current_user(&pool, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    if user.role != Role::Submissive {
        return Redirect::to("/dashboard").into_response();
    }

    let submissive_id = user.user_id.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = pool.get()?;
        let status = confinement::status_for(&conn, &submissive_id)?;
        let link_id = links::active_link_for_submissive(&conn, &submissive_id)?;
        let current_code = match &link_id {
            Some(link_id) => codes::current_unconsumed(&conn, link_id)?,
            None => None,
        };
        let recent = proofs::list_for_submissive(&conn, &submissive_id)?
            .into_iter()
            .take(5)
            .map(|s| RecentSubmission {
                status: s.status,
                kind: s.kind,
                submitted_ago: fmt_duration(session::now() - s.submitted_at) + " ago",
            })
            .collect::<Vec<_>>();
        Ok((status, current_code, recent))
    })
    .await;

    let Ok(Ok((status, current_code, recent))) = result else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    render(SubmissiveDashboardTemplate {
        initial: initial_of(&user.display_name),
        display_name: user.display_name,
        locked: status.locked,
        time_remaining_text: status.time_remaining_seconds.map(fmt_duration),
        overdue: status.overdue,
        clock_paused: status.clock_paused,
        clock_pause_message: status
            .session
            .as_ref()
            .and_then(|s| s.clock_pause_message.clone()),
        current_code: current_code.as_ref().map(|c| c.code.clone()),
        current_code_expires_text: current_code
            .as_ref()
            .map(|c| fmt_duration(c.expires_at - session::now())),
        recent,
    })
}

#[derive(Template)]
#[template(path = "submit_proof.html")]
struct SubmitProofTemplate {
    display_name: String,
    initial: String,
    current_code_id: Option<String>,
    current_code: Option<String>,
}

async fn submit_proof_page(State(pool): State<Pool>, jar: CookieJar) -> Response {
    let Some(user) = resolve_current_user(&pool, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    if user.role != Role::Submissive {
        return Redirect::to("/dashboard").into_response();
    }

    let submissive_id = user.user_id.clone();
    let current_code =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<codes::Code>> {
            let conn = pool.get()?;
            let Some(link_id) = links::active_link_for_submissive(&conn, &submissive_id)? else {
                return Ok(None);
            };
            Ok(codes::current_unconsumed(&conn, &link_id)?)
        })
        .await;

    let current_code = current_code.ok().and_then(|r| r.ok()).flatten();
    render(SubmitProofTemplate {
        initial: initial_of(&user.display_name),
        display_name: user.display_name,
        current_code_id: current_code.as_ref().map(|c| c.id.clone()),
        current_code: current_code.map(|c| c.code),
    })
}

struct DeviceRow {
    id: String,
    name: String,
    description: String,
    retired: bool,
}

#[derive(Template)]
#[template(path = "submissive_detail.html")]
struct SubmissiveDetailTemplate {
    submissive_id: String,
    display_name: String,
    devices: Vec<DeviceRow>,
    locked: bool,
    session_id: Option<String>,
    target_release_text: Option<String>,
    overdue: bool,
    clock_paused: bool,
    frequency_kind: String,
    keyholder_display_name: String,
    keyholder_initial: String,
    link_status: String,
    self_report_allowed: bool,
    catalog_visible_to_submissive: bool,
}

async fn submissive_detail_page(
    State(pool): State<Pool>,
    jar: CookieJar,
    Path(submissive_id): Path<String>,
) -> Response {
    let Some(user) = resolve_current_user(&pool, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    if user.role != Role::Keyholder {
        return Redirect::to("/submissive").into_response();
    }

    let keyholder_id = user.user_id.clone();
    let target_id = submissive_id.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<_>> {
        let conn = pool.get()?;
        let Some(link_id) =
            links::active_or_paused_link_for_keyholder(&conn, &keyholder_id, &target_id)?
        else {
            return Ok(None);
        };
        let display_name: String = conn.query_row(
            "SELECT display_name FROM users WHERE id = ?1",
            params![target_id],
            |row| row.get(0),
        )?;
        let device_list = devices::list(&conn, &target_id)?
            .into_iter()
            .map(|d| DeviceRow {
                id: d.id,
                name: d.name,
                description: d.description.unwrap_or_default(),
                retired: d.retired_at.is_some(),
            })
            .collect::<Vec<_>>();
        let status = confinement::status_for(&conn, &target_id)?;
        let p = policy::get_for_link(&conn, &link_id)?;
        let link_status: String = conn.query_row(
            "SELECT status FROM keyholder_submissive_links WHERE id = ?1",
            params![link_id],
            |row| row.get(0),
        )?;
        let settings = links::settings_for_link(&conn, &link_id)?;
        Ok(Some((
            display_name,
            device_list,
            status,
            p,
            link_status,
            settings,
        )))
    })
    .await;

    let Ok(Ok(Some((display_name, device_list, status, p, link_status, settings)))) = result else {
        return StatusCode::NOT_FOUND.into_response();
    };

    render(SubmissiveDetailTemplate {
        submissive_id,
        display_name,
        devices: device_list,
        locked: status.locked,
        session_id: status.session.as_ref().map(|s| s.id.clone()),
        target_release_text: status.time_remaining_seconds.map(fmt_duration),
        overdue: status.overdue,
        clock_paused: status.clock_paused,
        frequency_kind: p.map(|p| p.frequency_kind).unwrap_or_default(),
        link_status,
        self_report_allowed: settings.self_report_allowed,
        catalog_visible_to_submissive: settings.catalog_visible_to_submissive,
        keyholder_initial: initial_of(&user.display_name),
        keyholder_display_name: user.display_name,
    })
}

struct ReviewQueueItem {
    id: String,
    kind: String,
    purpose: String,
    submitted_ago: String,
    code: Option<String>,
    attachment_id: Option<String>,
    attachment_mime: Option<String>,
}

#[derive(Template)]
#[template(path = "proof_review.html")]
struct ProofReviewTemplate {
    pending: Vec<ReviewQueueItem>,
    pending_is_empty: bool,
    display_name: String,
    initial: String,
}

async fn review_queue_page(State(pool): State<Pool>, jar: CookieJar) -> Response {
    let Some(user) = resolve_current_user(&pool, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    if user.role != Role::Keyholder {
        return Redirect::to("/submissive").into_response();
    }

    let keyholder_id = user.user_id.clone();
    let pending = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ReviewQueueItem>> {
        let conn = pool.get()?;
        let link_ids = links::active_link_ids_for_keyholder(&conn, &keyholder_id)?;
        let submissions = proofs::list_for_links(&conn, &link_ids)?;
        let now = session::now();
        let mut items = Vec::new();
        for s in submissions.into_iter().filter(|s| s.status == "pending") {
            let attachments = proofs::list_attachments(&conn, &s.id)?;
            let first = attachments.into_iter().next();
            items.push(ReviewQueueItem {
                id: s.id,
                kind: s.kind,
                purpose: s.purpose,
                submitted_ago: fmt_duration(now - s.submitted_at) + " ago",
                code: s.verification_code_value,
                attachment_id: first.as_ref().map(|a| a.id.clone()),
                attachment_mime: first.as_ref().map(|a| a.mime_type.clone()),
            });
        }
        Ok(items)
    })
    .await;

    let Ok(Ok(pending)) = pending else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    render(ProofReviewTemplate {
        pending_is_empty: pending.is_empty(),
        pending,
        initial: initial_of(&user.display_name),
        display_name: user.display_name,
    })
}

#[derive(Template)]
#[template(path = "safety_alerts.html")]
struct SafetyAlertsTemplate {
    display_name: String,
    initial: String,
}

/// `/keyholder/safety-alerts` — everything is fetched client-side from
/// the JSON API this page already talks to; the template only needs
/// enough to render the nav shell (03-api-design.md §5).
async fn safety_alerts_page(State(pool): State<Pool>, jar: CookieJar) -> Response {
    let Some(user) = resolve_current_user(&pool, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    if user.role != Role::Keyholder {
        return Redirect::to("/submissive").into_response();
    }

    render(SafetyAlertsTemplate {
        initial: initial_of(&user.display_name),
        display_name: user.display_name,
    })
}

struct Badge {
    text: String,
    class: &'static str,
}

struct TemplateRow {
    id: String,
    title: String,
    description: Option<String>,
    active: bool,
    badges: Vec<Badge>,
}

struct KindGroup {
    kind: &'static str,
    label: &'static str,
    items: Vec<TemplateRow>,
}

const PILL_SKY: &str = "text-[11px] bg-sky-500/10 text-sky-400 px-2 py-0.5 rounded-full";
const PILL_SLATE: &str = "text-[11px] bg-slate-800 text-slate-400 px-2 py-0.5 rounded-full";
const PILL_SLATE_LIGHT: &str = "text-[11px] bg-slate-800 text-slate-300 px-2 py-0.5 rounded-full";
const PILL_EMERALD: &str =
    "text-[11px] bg-emerald-500/10 text-emerald-400 px-2 py-0.5 rounded-full";
const PILL_RED: &str = "text-[11px] bg-red-500/10 text-red-400 px-2 py-0.5 rounded-full";
const PILL_AMBER: &str = "text-[11px] bg-amber-500/10 text-amber-400 px-2 py-0.5 rounded-full";
const TEXT_MUTED: &str = "text-[11px] text-slate-500";
const TEXT_SUCCESS_CHAIN: &str = "text-[11px] text-emerald-400/90";
const TEXT_FAILURE_CHAIN: &str = "text-[11px] text-red-400/90";

/// The chip row under each catalog entry (mockups/catalog.html) — kind-
/// appropriate badges built from whatever fields the template actually
/// has set, plus the escalation chain (resolved to the linked template's
/// title via `titles`) for tasks.
fn template_badges(
    t: &templates::Template,
    titles: &std::collections::HashMap<String, String>,
) -> Vec<Badge> {
    let mut badges = Vec::new();
    match t.kind.as_str() {
        "task" => {
            match t.completion_type.as_deref() {
                Some("proof_required") => {
                    let media = t
                        .proof_media_types
                        .as_deref()
                        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                        .map(|v| v.join(", "))
                        .unwrap_or_default();
                    badges.push(Badge {
                        text: format!("Proof required · {media}"),
                        class: PILL_SKY,
                    });
                }
                _ => badges.push(Badge {
                    text: "Acknowledge only".to_string(),
                    class: PILL_SLATE_LIGHT,
                }),
            }
            if let Some(secs) = t.default_deadline_seconds {
                badges.push(Badge {
                    text: format!("{} deadline", fmt_duration(secs)),
                    class: PILL_SLATE,
                });
            }
            if t.points_delta.is_some_and(|p| p != 0) {
                badges.push(Badge {
                    text: format!("+{} pts", t.points_delta.unwrap()),
                    class: PILL_EMERALD,
                });
            }
            if t.on_success_template_id.is_none() && t.on_failure_template_id.is_none() {
                badges.push(Badge {
                    text: "no automatic escalation".to_string(),
                    class: TEXT_MUTED,
                });
            } else {
                if let Some(id) = &t.on_success_template_id {
                    let title = titles.get(id).cloned().unwrap_or_else(|| "?".to_string());
                    badges.push(Badge {
                        text: format!("✓ succeeds into: {title}"),
                        class: TEXT_SUCCESS_CHAIN,
                    });
                }
                if let Some(id) = &t.on_failure_template_id {
                    let title = titles.get(id).cloned().unwrap_or_else(|| "?".to_string());
                    badges.push(Badge {
                        text: format!("↳ fails into: {title}"),
                        class: TEXT_FAILURE_CHAIN,
                    });
                }
            }
        }
        "reward" => {
            match t.effect_kind.as_deref() {
                Some("time_reduction") => badges.push(Badge {
                    text: format!(
                        "reduces lock timer by {}",
                        fmt_duration(t.time_reduction_seconds.unwrap_or(0))
                    ),
                    class: PILL_EMERALD,
                }),
                _ => badges.push(Badge {
                    text: "direct grant".to_string(),
                    class: PILL_SLATE,
                }),
            }
            if let Some(cost) = t.points_cost {
                badges.push(Badge {
                    text: format!("redeemable for {cost} pts"),
                    class: PILL_AMBER,
                });
            }
        }
        "punishment" => match t.effect_kind.as_deref() {
            Some("time_extension") => badges.push(Badge {
                text: format!(
                    "extends lock timer by {}",
                    fmt_duration(t.time_extension_seconds.unwrap_or(0))
                ),
                class: PILL_RED,
            }),
            _ => badges.push(Badge {
                text: "direct grant".to_string(),
                class: PILL_SLATE,
            }),
        },
        _ => {}
    }
    badges
}

#[derive(Template)]
#[template(path = "catalog.html")]
struct CatalogTemplate {
    kinds: Vec<KindGroup>,
    display_name: String,
    initial: String,
}

async fn catalog_page(State(pool): State<Pool>, jar: CookieJar) -> Response {
    let Some(user) = resolve_current_user(&pool, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    if user.role != Role::Keyholder {
        return Redirect::to("/submissive").into_response();
    }

    let keyholder_id = user.user_id.clone();
    let all = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<templates::Template>> {
        let conn = pool.get()?;
        Ok(templates::list_for_keyholder(&conn, &keyholder_id, None)?)
    })
    .await;

    let Ok(Ok(all)) = all else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let titles: std::collections::HashMap<String, String> = all
        .iter()
        .map(|t| (t.id.clone(), t.title.clone()))
        .collect();

    let mut kinds = Vec::new();
    for (label, kind) in [
        ("Tasks", "task"),
        ("Punishments", "punishment"),
        ("Rewards", "reward"),
    ] {
        let items = all
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| TemplateRow {
                id: t.id.clone(),
                title: t.title.clone(),
                description: t.description.clone(),
                active: t.active,
                badges: template_badges(t, &titles),
            })
            .collect();
        kinds.push(KindGroup { kind, label, items });
    }

    render(CatalogTemplate {
        kinds,
        initial: initial_of(&user.display_name),
        display_name: user.display_name,
    })
}

#[derive(Template)]
#[template(path = "account_settings.html")]
struct AccountSettingsTemplate {
    display_name: String,
    initial: String,
    is_keyholder: bool,
    home_url: &'static str,
}

/// `/account` — session self-management, 2FA, and (Keyholder-only) API
/// tokens (10-operations.md §§1–2, 03-api-design.md §12). Everything on
/// the page is fetched client-side from the JSON API it already talks
/// to; the template only needs enough to render the nav shell.
async fn account_settings_page(State(pool): State<Pool>, jar: CookieJar) -> Response {
    let Some(user) = resolve_current_user(&pool, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    let is_keyholder = user.role == Role::Keyholder;
    render(AccountSettingsTemplate {
        initial: initial_of(&user.display_name),
        display_name: user.display_name,
        is_keyholder,
        home_url: if is_keyholder {
            "/dashboard"
        } else {
            "/submissive"
        },
    })
}

#[derive(Template)]
#[template(path = "assignment_proof.html")]
struct AssignmentProofTemplate {
    assignment_id: String,
    title: String,
    accepted_media: String,
    media_options: Vec<String>,
    display_name: String,
    initial: String,
}

async fn assignment_proof_page(
    State(pool): State<Pool>,
    jar: CookieJar,
    Path(assignment_id): Path<String>,
) -> Response {
    let Some(user) = resolve_current_user(&pool, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    if user.role != Role::Submissive {
        return Redirect::to("/dashboard").into_response();
    }

    let target_id = assignment_id.clone();
    let result =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<assignments::Assignment>> {
            let conn = pool.get()?;
            Ok(assignments::get(&conn, &target_id)?)
        })
        .await;

    let Ok(Ok(Some(a))) = result else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let media_options: Vec<String> = a
        .proof_media_types
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| vec!["photo".to_string()]);

    render(AssignmentProofTemplate {
        assignment_id,
        title: a.title,
        accepted_media: media_options.join(", "),
        media_options,
        initial: initial_of(&user.display_name),
        display_name: user.display_name,
    })
}

pub fn router() -> axum::Router<db::AppState> {
    use axum::routing::get;
    axum::Router::new()
        .route("/", get(index))
        .route("/login", get(login_page))
        .route("/invites/redeem", get(redeem_invite_page))
        .route("/dashboard", get(dashboard_page))
        .route("/submissive", get(submissive_dashboard_page))
        .route("/submissive/submit-proof", get(submit_proof_page))
        .route(
            "/submissive/assignments/{id}/proof",
            get(assignment_proof_page),
        )
        .route("/keyholder/submissives/{id}", get(submissive_detail_page))
        .route("/keyholder/review", get(review_queue_page))
        .route("/keyholder/safety-alerts", get(safety_alerts_page))
        .route("/keyholder/catalog", get(catalog_page))
        .route("/account", get(account_settings_page))
}
