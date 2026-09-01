//! Server-rendered pages (07-tech-stack.md §3): askama templates,
//! progressively enhanced with jQuery calling the `/api/v1` JSON routes
//! built in `api/`. Thin over the same domain layer the API uses — page
//! handlers read data to render, they don't duplicate business logic.

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use rusqlite::params;

use crate::auth::session::{self, CurrentUser, Role, SESSION_COOKIE_NAME};
use crate::db::Pool;

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

async fn index(State(pool): State<Pool>, jar: CookieJar) -> Redirect {
    match resolve_current_user(&pool, &jar).await {
        Some(_) => Redirect::to("/dashboard"),
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
        // No submissive dashboard exists yet — see the login/redeem
        // pages' inline note. Bouncing back to /login avoids stranding
        // them on a page built for the other role.
        return Redirect::to("/login").into_response();
    }

    let keyholder_id = user.user_id.clone();
    let roster = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<RosterRow>> {
        let conn = pool.get()?;
        let now = session::now();
        let mut stmt = conn.prepare(
            "SELECT u.display_name, l.started_at
             FROM keyholder_submissive_links l
             JOIN users u ON u.id = l.submissive_id
             WHERE l.keyholder_id = ?1 AND l.status = 'active'
             ORDER BY l.started_at DESC",
        )?;
        let rows = stmt
            .query_map(params![keyholder_id], |row| {
                let display_name: String = row.get(0)?;
                let started_at: i64 = row.get(1)?;
                Ok(RosterRow {
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

pub fn router() -> axum::Router<Pool> {
    use axum::routing::get;
    axum::Router::new()
        .route("/", get(index))
        .route("/login", get(login_page))
        .route("/invites/redeem", get(redeem_invite_page))
        .route("/dashboard", get(dashboard_page))
}
