mod api;
mod auth;
mod db;
mod domain;
mod live;
mod notify;
mod ops;
mod storage;
mod web;

use std::io::Write;

use axum::Router;
use axum::routing::get;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "owners-cock-ledger")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Local-host-only account recovery/bootstrap commands
    /// (10-operations.md §5) — never HTTP-reachable.
    Admin {
        #[command(subcommand)]
        action: AdminCommand,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Creates the first Keyholder account a fresh deployment needs
    /// before invite-based signup can start (10-operations.md §5).
    CreateKeyholder {
        email: String,
        #[arg(long)]
        display_name: Option<String>,
        /// Skip the "type the email back to confirm" prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Issues a single-use password-reset token, printed once
    /// (10-operations.md §5). Never sets a password itself.
    ResetPassword {
        email: String,
        #[arg(long)]
        yes: bool,
    },
    /// Force-clears 2FA for an account — the last resort for a lost
    /// device with exhausted recovery codes (10-operations.md §5).
    Disable2fa {
        email: String,
        #[arg(long)]
        yes: bool,
    },
    /// Clears a login lockout immediately (10-operations.md §5).
    UnlockAccount {
        email: String,
        #[arg(long)]
        yes: bool,
    },
    /// Ends a link unilaterally — the escape hatch for a Keyholder who
    /// never responds to an end-link request (06-future-extensions.md
    /// §2, 10-operations.md §5).
    ForceEndLink {
        link_id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Performs a live, safe backup of the database and blob directory
    /// into one output directory (10-operations.md §4).
    Backup {
        #[arg(long)]
        out: std::path::PathBuf,
    },
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

fn open_pool() -> anyhow::Result<db::Pool> {
    let data_dir = db::resolve_data_dir()?;
    std::fs::create_dir_all(&data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    tracing::info!(path = %data_dir.display(), "using data directory");

    let pool = db::init(&data_dir.join("db.sqlite3"))?;
    tracing::info!("migrations applied");
    Ok(pool)
}

fn build_router(state: db::AppState) -> Router {
    Router::new()
        .route("/health", get(ops::health))
        .nest(
            "/api/v1",
            api::auth::router()
                .merge(api::invites::router())
                .merge(api::roster::router())
                .merge(api::safety::router())
                .merge(api::chastity::router())
                .merge(api::verification::router())
                .merge(api::proofs::router())
                .merge(api::profiles::router())
                .merge(api::templates::router())
                .merge(api::assignments::router())
                .merge(api::api_tokens::router())
                .merge(api::notifications::router())
                .merge(api::toys::router())
                .merge(api::points::router())
                .merge(api::checkins::router())
                .merge(api::play_sessions::router())
                .merge(api::limits::router())
                .merge(api::recurring_tasks::router())
                .merge(api::stats::router()),
        )
        .merge(web::router())
        .nest_service(
            "/static",
            // No Cache-Control means the browser is free to reuse a stale
            // copy of app.css (or any static asset) for a heuristic
            // freshness window after a template/CSS edit, showing
            // unstyled/misrendered UI until the user happens to
            // hard-refresh. `no-cache` forces a cheap conditional
            // revalidation (If-Modified-Since -> 304 when unchanged) on
            // every request instead, so a rebuilt asset is picked up on
            // the very next load rather than "whenever the cache expires."
            tower::ServiceBuilder::new()
                .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-cache"),
                ))
                .service(tower_http::services::ServeDir::new("static")),
        )
        .layer(axum::middleware::from_fn(auth::csrf::csrf_protect))
        .with_state(state)
}

/// One tick of the verification-code issuance background task
/// (04-verification-workflow.md §2) — writes a `background_task_runs`
/// heartbeat regardless of outcome, same discipline as the health-check
/// design in `ops` (10-operations.md §3), so a silently-stopped task
/// shows up as unhealthy rather than looking indistinguishable from
/// "nothing was due."
fn run_verification_issuance_tick(pool: &db::Pool) {
    let conn = match pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            tracing::error!(error = %e, "verification issuance: failed to get a DB connection");
            return;
        }
    };
    match domain::verification::codes::run_due_issuance_tick(&conn) {
        Ok(issued) => {
            let _ = ops::record_heartbeat(&conn, "verification_issuance", true, None, issued);
            if issued > 0 {
                tracing::info!(issued, "verification codes issued");
            }
        }
        Err(e) => {
            let _ = ops::record_heartbeat(
                &conn,
                "verification_issuance",
                false,
                Some(&e.to_string()),
                0,
            );
            tracing::error!(error = %e, "verification issuance tick failed");
        }
    }
}

fn spawn_verification_issuance_task(pool: db::Pool) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let pool = pool.clone();
            if let Err(e) =
                tokio::task::spawn_blocking(move || run_verification_issuance_tick(&pool)).await
            {
                tracing::error!(error = %e, "verification issuance task panicked");
            }
        }
    });
}

/// One tick of the task-deadline sweeper (08-punishments-and-
/// deadlines.md §3: auto-fail pass, then the deadline-approaching
/// reminder pass), plus — on the same tick, per §9's own instruction
/// not to spin up a third background task — the confinement
/// still-paused reminder. Same heartbeat discipline as verification
/// issuance; `rows_processed` counts auto-failed tasks, matching what
/// it always has.
fn run_deadline_sweep_tick(pool: &db::Pool) {
    let mut conn = match pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            tracing::error!(error = %e, "deadline sweep: failed to get a DB connection");
            return;
        }
    };
    match domain::rewards_punishments::assignments::run_deadline_sweep_tick(&mut conn) {
        Ok(outcome) => {
            let failed = outcome.auto_failed.len() as i64;
            let _ = ops::record_heartbeat(&conn, "deadline_sweeper", true, None, failed);
            if failed > 0 {
                tracing::info!(failed, "tasks auto-failed on deadline");
            }

            for task in outcome.auto_failed {
                for recipient in [&task.submissive_id, &task.keyholder_id] {
                    let _ = notify::notify_sync(
                        pool,
                        &conn,
                        notify::Event {
                            user_id: recipient,
                            link_id: Some(&task.link_id),
                            notification_type: "task.failed",
                            title: "A task deadline passed",
                            body: None,
                            link_path: Some("/submissive"),
                            related_entity_type: Some("assignments"),
                            related_entity_id: Some(&task.assignment_id),
                            push: true,
                        },
                    );
                }
                if let Some(escalated) = &task.escalated {
                    let pool = pool.clone();
                    let keyholder_id = task.keyholder_id.clone();
                    let submissive_id = task.submissive_id.clone();
                    let escalated = escalated.clone();
                    tokio::spawn(async move {
                        api::assignments::notify_for_assignment(
                            &pool,
                            &keyholder_id,
                            &submissive_id,
                            &escalated,
                            true,
                        )
                        .await;
                    });
                }
            }

            for reminder in outcome.reminders {
                let _ = notify::notify_sync(
                    pool,
                    &conn,
                    notify::Event {
                        user_id: &reminder.submissive_id,
                        link_id: None,
                        notification_type: "task.deadline_approaching",
                        title: &format!("Deadline approaching: {}", reminder.title),
                        body: None,
                        link_path: Some("/submissive"),
                        related_entity_type: Some("assignments"),
                        related_entity_id: Some(&reminder.assignment_id),
                        push: true,
                    },
                );
            }
        }
        Err(e) => {
            let _ =
                ops::record_heartbeat(&conn, "deadline_sweeper", false, Some(&e.to_string()), 0);
            tracing::error!(error = %e, "deadline sweep tick failed");
        }
    }

    match domain::chastity::confinement::run_still_paused_sweep_tick(&conn) {
        Ok(reminders) => {
            for reminder in reminders {
                let _ = notify::notify_sync(
                    pool,
                    &conn,
                    notify::Event {
                        user_id: &reminder.keyholder_id,
                        link_id: None,
                        notification_type: "confinement.clock_still_paused",
                        title: "A submissive's lock timer is still paused",
                        body: None,
                        link_path: None,
                        related_entity_type: Some("confinement_sessions"),
                        related_entity_id: Some(&reminder.session_id),
                        push: true,
                    },
                );
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "still-paused sweep tick failed");
        }
    }

    match domain::links::run_end_request_escalation_sweep_tick(&conn) {
        Ok(escalations) => {
            for req in escalations {
                let _ = notify::notify_sync(
                    pool,
                    &conn,
                    notify::Event {
                        user_id: &req.keyholder_id,
                        link_id: Some(&req.link_id),
                        notification_type: "link.end_request_reminder",
                        title: "Still waiting on your response to an end request",
                        body: req.reason.as_deref(),
                        link_path: None,
                        related_entity_type: Some("keyholder_submissive_links"),
                        related_entity_id: Some(&req.link_id),
                        push: true,
                    },
                );
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "end-request escalation sweep tick failed");
        }
    }

    match domain::links::run_oversight_still_paused_sweep_tick(&conn) {
        Ok(reminders) => {
            for reminder in reminders {
                let _ = notify::notify_sync(
                    pool,
                    &conn,
                    notify::Event {
                        user_id: &reminder.keyholder_id,
                        link_id: Some(&reminder.link_id),
                        notification_type: "oversight.still_paused",
                        title: "Oversight is still paused for a submissive",
                        body: None,
                        link_path: None,
                        related_entity_type: Some("keyholder_submissive_links"),
                        related_entity_id: Some(&reminder.link_id),
                        push: true,
                    },
                );
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "oversight still-paused sweep tick failed");
        }
    }

    match domain::recurring_tasks::run_recurring_task_sweep_tick(&mut conn) {
        Ok(spawned) => {
            for task in spawned {
                let pool = pool.clone();
                let keyholder_id = task.keyholder_id.clone();
                let submissive_id = task.submissive_id.clone();
                let assignment = task.assignment.clone();
                tokio::spawn(async move {
                    api::assignments::notify_for_assignment(
                        &pool,
                        &keyholder_id,
                        &submissive_id,
                        &assignment,
                        false,
                    )
                    .await;
                });
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "recurring task sweep tick failed");
        }
    }
}

fn spawn_deadline_sweeper_task(pool: db::Pool) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let pool = pool.clone();
            if let Err(e) =
                tokio::task::spawn_blocking(move || run_deadline_sweep_tick(&pool)).await
            {
                tracing::error!(error = %e, "deadline sweeper task panicked");
            }
        }
    });
}

async fn serve(pool: db::Pool) -> anyhow::Result<()> {
    let blob_dir = db::resolve_data_dir()?.join("blobs");
    std::fs::create_dir_all(&blob_dir)?;

    spawn_verification_issuance_task(pool.clone());
    spawn_deadline_sweeper_task(pool.clone());

    let state = db::AppState {
        pool,
        blob_dir: db::BlobDir(blob_dir),
        play_session_streams: live::PlaySessionStreams::default(),
    };
    let app = build_router(state);

    let listen_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    tracing::info!(addr = %listen_addr, "listening");

    axum::serve(listener, app).await?;
    Ok(())
}

/// Shared confirmation friction for every admin recovery command
/// (10-operations.md §5): print what's about to happen, require typing
/// `echo_value` back, or skip entirely with `--yes` for scripted use.
fn confirm_or_bail(prompt: &str, echo_value: &str, yes: bool) -> anyhow::Result<()> {
    if yes {
        return Ok(());
    }
    print!("{prompt} Type '{echo_value}' to confirm: ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if input.trim() != echo_value {
        anyhow::bail!("confirmation did not match — aborted, nothing changed");
    }
    Ok(())
}

/// `admin create-keyholder` (10-operations.md §5): the one CLI command
/// that creates an account rather than recovering one — meant to run
/// exactly once per deployment, before any invite can exist.
fn admin_create_keyholder(
    pool: db::Pool,
    email: String,
    display_name: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    confirm_or_bail(
        &format!("Create a keyholder account for '{email}'?"),
        &email,
        yes,
    )?;

    let display_name = display_name.unwrap_or_else(|| "Keyholder".to_string());
    let temp_password = auth::token::generate();
    let password_hash = auth::password::hash_password(&temp_password)?;

    let conn = pool.get()?;
    let user_id = domain::users::create_keyholder(
        &conn,
        domain::users::NewAccount {
            email: &email,
            password_hash: &password_hash,
            display_name: &display_name,
        },
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    domain::audit::record(
        &conn,
        domain::audit::Entry {
            actor: domain::audit::Actor::AdminCli,
            link_id: None,
            action: "user.created_via_admin_cli",
            entity_type: "users",
            entity_id: &user_id,
            detail: None,
        },
    )?;

    println!("Created keyholder account for {email} (id: {user_id}).");
    println!("Temporary password (shown once): {temp_password}");
    println!("Relay this to the account holder and have them change it after first login.");
    Ok(())
}

fn find_account_or_bail(conn: &rusqlite::Connection, email: &str) -> anyhow::Result<String> {
    domain::users::find_by_email(conn, email)?
        .map(|a| a.id)
        .ok_or_else(|| anyhow::anyhow!("no account found for {email}"))
}

/// `admin reset-password <email>` (10-operations.md §5): issues a
/// single-use reset token and prints it once — never sets a password
/// itself, mirroring invite redemption.
fn admin_reset_password(pool: db::Pool, email: String, yes: bool) -> anyhow::Result<()> {
    confirm_or_bail(
        &format!("Issue a password reset token for '{email}'?"),
        &email,
        yes,
    )?;

    let conn = pool.get()?;
    let user_id = find_account_or_bail(&conn, &email)?;
    let issued = domain::password_reset::issue(
        &conn,
        &user_id,
        domain::password_reset::RequestedVia::AdminCli,
    )?;

    domain::audit::record(
        &conn,
        domain::audit::Entry {
            actor: domain::audit::Actor::AdminCli,
            link_id: None,
            action: "user.password_reset_issued_via_admin_cli",
            entity_type: "users",
            entity_id: &user_id,
            detail: None,
        },
    )?;

    println!(
        "Password reset token for {email} (shown once): {}",
        issued.token
    );
    println!(
        "Expires at {}. Relay it to the account holder — they redeem it at /password-reset/redeem.",
        api::iso8601(issued.expires_at)
    );
    Ok(())
}

/// `admin disable-2fa <email>` (10-operations.md §5): the last resort
/// for a lost authenticator device with exhausted recovery codes.
fn admin_disable_2fa(pool: db::Pool, email: String, yes: bool) -> anyhow::Result<()> {
    confirm_or_bail(&format!("Force-disable 2FA for '{email}'?"), &email, yes)?;

    let conn = pool.get()?;
    let user_id = find_account_or_bail(&conn, &email)?;
    domain::two_factor::force_disable_for_user(&conn, &user_id)?;

    domain::audit::record(
        &conn,
        domain::audit::Entry {
            actor: domain::audit::Actor::AdminCli,
            link_id: None,
            action: "user.two_factor_disabled_via_admin_cli",
            entity_type: "users",
            entity_id: &user_id,
            detail: None,
        },
    )?;

    println!("2FA disabled for {email}.");
    Ok(())
}

/// `admin unlock-account <email>` (10-operations.md §5): a convenience,
/// not a necessity — the account already self-unlocks once
/// `locked_until` passes.
fn admin_unlock_account(pool: db::Pool, email: String, yes: bool) -> anyhow::Result<()> {
    confirm_or_bail(&format!("Unlock the account for '{email}'?"), &email, yes)?;

    let conn = pool.get()?;
    let user_id = find_account_or_bail(&conn, &email)?;
    domain::users::unlock(&conn, &user_id)?;

    domain::audit::record(
        &conn,
        domain::audit::Entry {
            actor: domain::audit::Actor::AdminCli,
            link_id: None,
            action: "user.unlocked_via_admin_cli",
            entity_type: "users",
            entity_id: &user_id,
            detail: None,
        },
    )?;

    println!("Account for {email} unlocked.");
    Ok(())
}

/// `admin force-end-link <link_id>` (10-operations.md §5,
/// 06-future-extensions.md §2): the Tier 2 escape hatch for a Keyholder
/// who never responds to an end-link request.
fn admin_force_end_link(pool: db::Pool, link_id: String, yes: bool) -> anyhow::Result<()> {
    confirm_or_bail(&format!("Force-end link '{link_id}'?"), &link_id, yes)?;

    let conn = pool.get()?;
    if !domain::links::force_end(&conn, &link_id)? {
        anyhow::bail!("no active or paused link found with id {link_id}");
    }

    domain::audit::record(
        &conn,
        domain::audit::Entry {
            actor: domain::audit::Actor::AdminCli,
            link_id: Some(&link_id),
            action: "link.force_ended_via_admin_cli",
            entity_type: "keyholder_submissive_links",
            entity_id: &link_id,
            detail: None,
        },
    )?;

    println!("Link {link_id} ended.");
    Ok(())
}

/// `admin backup --out <dir>` (10-operations.md §4): a live, online-safe
/// copy of the SQLite database (via SQLite's own backup API, safe under
/// WAL against a concurrently-running server) plus the blob directory,
/// into one output directory. Not a background task or HTTP endpoint —
/// the deployer wires this into cron/systemd themselves.
fn admin_backup(pool: db::Pool, out: std::path::PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(&out)?;

    let conn = pool.get()?;
    let db_dest = out.join("db.sqlite3");
    conn.backup(rusqlite::MAIN_DB, &db_dest, None)?;
    println!("Database backed up to {}", db_dest.display());

    let blob_src = db::resolve_data_dir()?.join("blobs");
    let blob_dest = out.join("blobs");
    if blob_src.is_dir() {
        copy_dir_recursive(&blob_src, &blob_dest)?;
        println!("Blob directory backed up to {}", blob_dest.display());
    } else {
        std::fs::create_dir_all(&blob_dest)?;
    }

    println!("Backup complete: {}", out.display());
    Ok(())
}

fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let pool = open_pool()?;

    match cli.command {
        None => serve(pool).await,
        Some(Command::Admin { action }) => match action {
            AdminCommand::CreateKeyholder {
                email,
                display_name,
                yes,
            } => admin_create_keyholder(pool, email, display_name, yes),
            AdminCommand::ResetPassword { email, yes } => admin_reset_password(pool, email, yes),
            AdminCommand::Disable2fa { email, yes } => admin_disable_2fa(pool, email, yes),
            AdminCommand::UnlockAccount { email, yes } => admin_unlock_account(pool, email, yes),
            AdminCommand::ForceEndLink { link_id, yes } => admin_force_end_link(pool, link_id, yes),
            AdminCommand::Backup { out } => admin_backup(pool, out),
        },
    }
}

/// End-to-end tests against the real router (not just individual domain
/// functions) — login, CSRF, invite redemption, and roster listing all
/// only prove anything once they're exercised together over HTTP the way
/// a browser actually would.
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use std::collections::HashMap;
    use tower::ServiceExt;

    struct TestClient {
        app: Router,
        cookies: HashMap<String, String>,
        // Owned only when this client made its own blob dir (`new`); a
        // client sharing one via `new_with_blob_dir` (two TestClients
        // standing in for two real browsers hitting the same server)
        // holds None and relies on the caller keeping the shared TempDir
        // alive instead.
        _blob_dir: Option<tempfile::TempDir>,
    }

    fn test_app_state(
        pool: db::Pool,
        blob_dir: std::path::PathBuf,
        play_session_streams: live::PlaySessionStreams,
    ) -> Router {
        let state = db::AppState {
            pool,
            blob_dir: db::BlobDir(blob_dir),
            play_session_streams,
        };
        build_router(state)
    }

    impl TestClient {
        fn new(pool: db::Pool) -> Self {
            let blob_dir = tempfile::tempdir().unwrap();
            let app = test_app_state(
                pool,
                blob_dir.path().to_path_buf(),
                live::PlaySessionStreams::default(),
            );
            Self {
                app,
                cookies: HashMap::new(),
                _blob_dir: Some(blob_dir),
            }
        }

        /// For two `TestClient`s standing in for two different people
        /// hitting the same running server — they must share one blob
        /// dir and one `PlaySessionStreams` registry, the way one real
        /// process's `AppState` does, or a file one of them uploads (or
        /// a live SSE event one of them publishes) is invisible to the
        /// other.
        fn new_with_blob_dir(
            pool: db::Pool,
            blob_dir: &std::path::Path,
            play_session_streams: live::PlaySessionStreams,
        ) -> Self {
            let app = test_app_state(pool, blob_dir.to_path_buf(), play_session_streams);
            Self {
                app,
                cookies: HashMap::new(),
                _blob_dir: None,
            }
        }

        fn cookie_header(&self) -> String {
            self.cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ")
        }

        async fn request(
            &mut self,
            method: &str,
            path: &str,
            body: Option<serde_json::Value>,
        ) -> (StatusCode, serde_json::Value) {
            let mut builder = Request::builder().method(method).uri(path);
            if !self.cookies.is_empty() {
                builder = builder.header(header::COOKIE, self.cookie_header());
            }
            if method != "GET"
                && let Some(csrf) = self.cookies.get(auth::csrf::CSRF_COOKIE_NAME)
            {
                builder = builder.header(auth::csrf::CSRF_HEADER_NAME, csrf.clone());
            }
            let body = match body {
                Some(v) => {
                    builder = builder.header(header::CONTENT_TYPE, "application/json");
                    Body::from(v.to_string())
                }
                None => Body::empty(),
            };
            let response = self
                .app
                .clone()
                .oneshot(builder.body(body).unwrap())
                .await
                .unwrap();
            let status = response.status();
            self.capture_cookies(&response);

            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let json = if bytes.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
            };
            (status, json)
        }

        async fn get(&mut self, path: &str) -> (StatusCode, serde_json::Value) {
            self.request("GET", path, None).await
        }

        /// For HTML pages and redirects, where the body isn't JSON and the
        /// `Location` header matters.
        async fn get_page(&mut self, path: &str) -> (StatusCode, Option<String>, String) {
            let mut builder = Request::builder().method("GET").uri(path);
            if !self.cookies.is_empty() {
                builder = builder.header(header::COOKIE, self.cookie_header());
            }
            let response = self
                .app
                .clone()
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (
                status,
                location,
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        }

        async fn post(
            &mut self,
            path: &str,
            body: serde_json::Value,
        ) -> (StatusCode, serde_json::Value) {
            self.request("POST", path, Some(body)).await
        }

        async fn patch(
            &mut self,
            path: &str,
            body: serde_json::Value,
        ) -> (StatusCode, serde_json::Value) {
            self.request("PATCH", path, Some(body)).await
        }

        async fn delete(&mut self, path: &str) -> (StatusCode, serde_json::Value) {
            self.request("DELETE", path, None).await
        }

        /// Bearer-token-authenticated requests, standing in for an
        /// external script using a Keyholder-issued API token
        /// (03-api-design.md §12) rather than a browser session — no
        /// cookies, no CSRF header, since Bearer requests are exempt.
        async fn request_bearer(
            &mut self,
            method: &str,
            path: &str,
            token: &str,
            body: Option<serde_json::Value>,
        ) -> (StatusCode, serde_json::Value) {
            let mut builder = Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
            let body = match body {
                Some(v) => {
                    builder = builder.header(header::CONTENT_TYPE, "application/json");
                    Body::from(v.to_string())
                }
                None => Body::empty(),
            };
            let response = self
                .app
                .clone()
                .oneshot(builder.body(body).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let json = if bytes.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
            };
            (status, json)
        }

        async fn get_bearer(&mut self, path: &str, token: &str) -> (StatusCode, serde_json::Value) {
            self.request_bearer("GET", path, token, None).await
        }

        async fn post_bearer(
            &mut self,
            path: &str,
            token: &str,
            body: serde_json::Value,
        ) -> (StatusCode, serde_json::Value) {
            self.request_bearer("POST", path, token, Some(body)).await
        }

        async fn delete_with_body(
            &mut self,
            path: &str,
            body: serde_json::Value,
        ) -> (StatusCode, serde_json::Value) {
            self.request("DELETE", path, Some(body)).await
        }

        fn capture_cookies(&mut self, response: &axum::http::Response<Body>) {
            for value in response.headers().get_all(header::SET_COOKIE) {
                let raw = value.to_str().unwrap();
                if let Some((k, v)) = raw.split(';').next().and_then(|kv| kv.split_once('=')) {
                    self.cookies
                        .insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }

        /// A minimal hand-built `multipart/form-data` body — proof
        /// submission is the only endpoint that needs one, so a tiny
        /// purpose-built builder here beats pulling in a multipart-client
        /// crate for one test path.
        async fn post_multipart(
            &mut self,
            path: &str,
            fields: &[(&str, &str)],
            files: &[(&str, &str, &str, &[u8])],
        ) -> (StatusCode, serde_json::Value) {
            let boundary = "test-boundary-owners-cock-ledger";
            let mut body = Vec::new();
            for (name, value) in fields {
                body.extend_from_slice(
                    format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                        .as_bytes(),
                );
            }
            for (field_name, filename, content_type, bytes) in files {
                body.extend_from_slice(
                    format!(
                        "--{boundary}\r\nContent-Disposition: form-data; name=\"{field_name}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(bytes);
                body.extend_from_slice(b"\r\n");
            }
            body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

            let mut builder = Request::builder().method("POST").uri(path).header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            );
            if !self.cookies.is_empty() {
                builder = builder.header(header::COOKIE, self.cookie_header());
            }
            if let Some(csrf) = self.cookies.get(auth::csrf::CSRF_COOKIE_NAME) {
                builder = builder.header(auth::csrf::CSRF_HEADER_NAME, csrf.clone());
            }

            let response = self
                .app
                .clone()
                .oneshot(builder.body(Body::from(body)).unwrap())
                .await
                .unwrap();
            let status = response.status();
            self.capture_cookies(&response);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let json = if bytes.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
            };
            (status, json)
        }
    }

    fn temp_pool() -> (tempfile::TempDir, db::Pool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("db.sqlite3")).unwrap();
        (dir, pool)
    }

    fn seed_keyholder(pool: &db::Pool, email: &str, password: &str) {
        let conn = pool.get().unwrap();
        let hash = auth::password::hash_password(password).unwrap();
        domain::users::create_keyholder(
            &conn,
            domain::users::NewAccount {
                email,
                password_hash: &hash,
                display_name: "KH",
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn full_login_flow_sets_a_session_cookie() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool);
        client.get("/health").await;

        let (status, body) = client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["role"], "keyholder");
        assert!(
            client
                .cookies
                .contains_key(auth::session::SESSION_COOKIE_NAME)
        );
    }

    #[tokio::test]
    async fn wrong_password_is_rejected() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh2@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool);
        client.get("/health").await;

        let (status, _) = client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh2@example.test", "password": "wrong"}),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn me_reflects_the_logged_in_user() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh3@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool);
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh3@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, body) = client.get("/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["display_name"], "KH");
    }

    #[tokio::test]
    async fn invite_create_redeem_and_roster_round_trip() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh4@example.test", "correct horse battery staple");
        let mut keyholder = TestClient::new(pool.clone());
        keyholder.get("/health").await;
        keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh4@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, body) = keyholder
            .post("/api/v1/keyholder/invites", serde_json::json!({}))
            .await;
        assert_eq!(status, StatusCode::OK);
        let token = body["token"].as_str().unwrap().to_string();

        let mut submissive = TestClient::new(pool.clone());
        submissive.get("/health").await;
        let (status, _) = submissive
            .post(
                "/api/v1/auth/invites/redeem",
                serde_json::json!({
                    "token": token,
                    "email": "new-sub@example.test",
                    "password": "another strong password",
                    "display_name": "New Sub",
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = keyholder.get("/api/v1/keyholder/submissives").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["display_name"], "New Sub");

        // Redeeming notifies the keyholder, feed-only (09-notifications.md §3).
        let (_, feed) = keyholder.get("/api/v1/notifications").await;
        let established = feed
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["type"] == "link.established")
            .expect("link.established notification");
        assert!(established["title"].as_str().unwrap().contains("New Sub"));
    }

    #[tokio::test]
    async fn submissive_cannot_create_invites() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh5@example.test", "correct horse battery staple");
        let mut keyholder = TestClient::new(pool.clone());
        keyholder.get("/health").await;
        keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh5@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (_, body) = keyholder
            .post("/api/v1/keyholder/invites", serde_json::json!({}))
            .await;
        let token = body["token"].as_str().unwrap().to_string();

        let mut submissive = TestClient::new(pool.clone());
        submissive.get("/health").await;
        submissive
            .post(
                "/api/v1/auth/invites/redeem",
                serde_json::json!({
                    "token": token,
                    "email": "sub5@example.test",
                    "password": "another strong password",
                    "display_name": "Sub5",
                }),
            )
            .await;

        let (status, _) = submissive
            .post("/api/v1/keyholder/invites", serde_json::json!({}))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn listing_sessions_shows_both_logins_with_current_flagged() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "sessions1@example.test",
            "correct horse battery staple",
        );

        let mut first = TestClient::new(pool.clone());
        first.get("/health").await;
        first
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "sessions1@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let mut second = TestClient::new(pool.clone());
        second.get("/health").await;
        second
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "sessions1@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, body) = first.get("/api/v1/auth/sessions").await;
        assert_eq!(status, StatusCode::OK);
        let sessions = body.as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(
            sessions.iter().filter(|s| s["is_current"] == true).count(),
            1
        );
    }

    #[tokio::test]
    async fn deleting_another_session_by_id_revokes_only_that_one() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "sessions2@example.test",
            "correct horse battery staple",
        );

        let mut first = TestClient::new(pool.clone());
        first.get("/health").await;
        first
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "sessions2@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let mut second = TestClient::new(pool.clone());
        second.get("/health").await;
        second
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "sessions2@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (_, body) = first.get("/api/v1/auth/sessions").await;
        let other_id = body
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["is_current"] == false)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        let (status, _) = first
            .delete(&format!("/api/v1/auth/sessions/{other_id}"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // The revoked session (second) can no longer use /auth/me.
        let (status, _) = second.get("/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The caller's own session (first) is untouched.
        let (status, _) = first.get("/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn cannot_delete_another_users_session_by_guessing_its_id() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "sessions3a@example.test",
            "correct horse battery staple",
        );
        seed_keyholder(
            &pool,
            "sessions3b@example.test",
            "correct horse battery staple",
        );

        let mut victim = TestClient::new(pool.clone());
        victim.get("/health").await;
        victim
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "sessions3a@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let victim_session_id = victim.cookies[auth::session::SESSION_COOKIE_NAME].clone();

        let mut attacker = TestClient::new(pool.clone());
        attacker.get("/health").await;
        attacker
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "sessions3b@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, _) = attacker
            .delete(&format!("/api/v1/auth/sessions/{victim_session_id}"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = victim.get("/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn revoking_all_sessions_except_current_leaves_only_the_caller_logged_in() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "sessions4@example.test",
            "correct horse battery staple",
        );

        let mut first = TestClient::new(pool.clone());
        first.get("/health").await;
        first
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "sessions4@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let mut second = TestClient::new(pool.clone());
        second.get("/health").await;
        second
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "sessions4@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, _) = first
            .delete_with_body(
                "/api/v1/auth/sessions",
                serde_json::json!({"except_current": true}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = second.get("/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = first.get("/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn changing_password_revokes_other_sessions_and_updates_login() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "changepw@example.test",
            "correct horse battery staple",
        );

        let mut first = TestClient::new(pool.clone());
        first.get("/health").await;
        first
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "changepw@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let mut second = TestClient::new(pool.clone());
        second.get("/health").await;
        second
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "changepw@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, _) = first
            .post(
                "/api/v1/auth/password/change",
                serde_json::json!({"current_password": "correct horse battery staple", "new_password": "a brand new password"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Other session revoked.
        let (status, _) = second.get("/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Caller's own session survives the change.
        let (status, _) = first.get("/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::OK);

        // New password actually works on a fresh login.
        let mut fresh = TestClient::new(pool.clone());
        fresh.get("/health").await;
        let (status, _) = fresh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "changepw@example.test", "password": "a brand new password"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn changing_password_with_wrong_current_password_is_rejected() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "changepw2@example.test",
            "correct horse battery staple",
        );
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "changepw2@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, _) = client
            .post(
                "/api/v1/auth/password/change",
                serde_json::json!({"current_password": "totally wrong", "new_password": "a brand new password"}),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    fn totp_code_for(secret_base32: &str, account_name: &str) -> String {
        use totp_rs::{Algorithm, Builder, Secret};
        let totp = Builder::new()
            .with_algorithm(Algorithm::SHA1)
            .with_digits(6)
            .with_skew(1)
            .with_step_duration(30)
            .with_secret(Secret::try_from_base32(secret_base32).unwrap())
            .with_issuer(Some("Owner's Cock Ledger"))
            .with_account_name(account_name)
            .build()
            .unwrap();
        totp.generate_current().to_string()
    }

    #[tokio::test]
    async fn two_factor_setup_confirm_and_login_challenge_round_trip() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "tfa1@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tfa1@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, status_body) = client.get("/api/v1/auth/2fa/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(status_body["enabled"], false);

        let (status, setup_body) = client
            .post("/api/v1/auth/2fa/setup", serde_json::json!({}))
            .await;
        assert_eq!(status, StatusCode::OK);
        let secret = setup_body["secret"].as_str().unwrap().to_string();
        assert!(
            setup_body["otpauth_uri"]
                .as_str()
                .unwrap()
                .starts_with("otpauth://")
        );
        assert!(!setup_body["qr_png_base64"].as_str().unwrap().is_empty());

        let code = totp_code_for(&secret, "tfa1@example.test");
        let (status, confirm_body) = client
            .post(
                "/api/v1/auth/2fa/confirm",
                serde_json::json!({"code": code}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let recovery_codes = confirm_body["recovery_codes"].as_array().unwrap();
        assert_eq!(recovery_codes.len(), 10);

        let (_, feed) = client.get("/api/v1/notifications").await;
        assert!(
            feed.as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "account.2fa_enabled")
        );

        let (status, status_body) = client.get("/api/v1/auth/2fa/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(status_body["enabled"], true);
        assert_eq!(status_body["recovery_codes_remaining"], 10);

        // A fresh login now returns a challenge instead of a session.
        let mut fresh = TestClient::new(pool.clone());
        fresh.get("/health").await;
        let (status, login_body) = fresh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tfa1@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(login_body["requires_2fa"], true);
        assert!(
            !fresh
                .cookies
                .contains_key(auth::session::SESSION_COOKIE_NAME)
        );
        let challenge_token = login_body["challenge_token"].as_str().unwrap().to_string();

        let login_code = totp_code_for(&secret, "tfa1@example.test");
        let (status, _) = fresh
            .post(
                "/api/v1/auth/2fa/verify",
                serde_json::json!({"challenge_token": challenge_token, "code": login_code}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            fresh
                .cookies
                .contains_key(auth::session::SESSION_COOKIE_NAME)
        );
    }

    #[tokio::test]
    async fn login_challenge_rejects_a_wrong_code() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "tfa2@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tfa2@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (_, setup_body) = client
            .post("/api/v1/auth/2fa/setup", serde_json::json!({}))
            .await;
        let secret = setup_body["secret"].as_str().unwrap().to_string();
        let code = totp_code_for(&secret, "tfa2@example.test");
        client
            .post(
                "/api/v1/auth/2fa/confirm",
                serde_json::json!({"code": code}),
            )
            .await;

        let mut fresh = TestClient::new(pool.clone());
        fresh.get("/health").await;
        let (_, login_body) = fresh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tfa2@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let challenge_token = login_body["challenge_token"].as_str().unwrap().to_string();

        let (status, _) = fresh
            .post(
                "/api/v1/auth/2fa/verify",
                serde_json::json!({"challenge_token": challenge_token, "code": "000000"}),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_recovery_code_completes_a_login_challenge() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "tfa3@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tfa3@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (_, setup_body) = client
            .post("/api/v1/auth/2fa/setup", serde_json::json!({}))
            .await;
        let secret = setup_body["secret"].as_str().unwrap().to_string();
        let code = totp_code_for(&secret, "tfa3@example.test");
        let (_, confirm_body) = client
            .post(
                "/api/v1/auth/2fa/confirm",
                serde_json::json!({"code": code}),
            )
            .await;
        let recovery_code = confirm_body["recovery_codes"][0]
            .as_str()
            .unwrap()
            .to_string();

        let mut fresh = TestClient::new(pool.clone());
        fresh.get("/health").await;
        let (_, login_body) = fresh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tfa3@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let challenge_token = login_body["challenge_token"].as_str().unwrap().to_string();

        let (status, _) = fresh
            .post(
                "/api/v1/auth/2fa/verify",
                serde_json::json!({"challenge_token": challenge_token, "code": recovery_code}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn disabling_2fa_requires_both_password_and_a_live_code() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "tfa4@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tfa4@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (_, setup_body) = client
            .post("/api/v1/auth/2fa/setup", serde_json::json!({}))
            .await;
        let secret = setup_body["secret"].as_str().unwrap().to_string();
        let code = totp_code_for(&secret, "tfa4@example.test");
        client
            .post(
                "/api/v1/auth/2fa/confirm",
                serde_json::json!({"code": code}),
            )
            .await;

        // Wrong password, valid-looking code shape: rejected.
        let (status, _) = client
            .post(
                "/api/v1/auth/2fa/disable",
                serde_json::json!({"current_password": "totally wrong", "code": "000000"}),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Right password, wrong code: still rejected.
        let (status, _) = client
            .post(
                "/api/v1/auth/2fa/disable",
                serde_json::json!({"current_password": "correct horse battery staple", "code": "000000"}),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            client.get("/api/v1/auth/2fa/status").await.1["enabled"],
            true
        );

        // Right password, right code: succeeds.
        let disable_code = totp_code_for(&secret, "tfa4@example.test");
        let (status, _) = client
            .post(
                "/api/v1/auth/2fa/disable",
                serde_json::json!({"current_password": "correct horse battery staple", "code": disable_code}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            client.get("/api/v1/auth/2fa/status").await.1["enabled"],
            false
        );

        let (_, feed) = client.get("/api/v1/notifications").await;
        assert!(
            feed.as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "account.2fa_disabled")
        );
    }

    #[tokio::test]
    async fn password_reset_request_always_returns_202_regardless_of_account_existence() {
        let (_dir, pool) = temp_pool();
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        let (status, _) = client
            .post(
                "/api/v1/auth/password-reset/request",
                serde_json::json!({"email": "nobody-at-all@example.test"}),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn admin_issued_reset_token_can_be_redeemed_and_revokes_old_sessions() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "reset1@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "reset1@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let raw_token = {
            let conn = pool.get().unwrap();
            let user_id = domain::users::find_by_email(&conn, "reset1@example.test")
                .unwrap()
                .unwrap()
                .id;
            domain::password_reset::issue(
                &conn,
                &user_id,
                domain::password_reset::RequestedVia::AdminCli,
            )
            .unwrap()
            .token
        };

        let (status, _) = client
            .post(
                "/api/v1/auth/password-reset/redeem",
                serde_json::json!({"token": raw_token, "new_password": "a totally different password"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // The session that existed before the reset is now revoked.
        let (status, _) = client.get("/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // The new password works; the old one doesn't.
        let mut fresh = TestClient::new(pool.clone());
        fresh.get("/health").await;
        let (status, _) = fresh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "reset1@example.test", "password": "a totally different password"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = fresh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "reset1@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn redeeming_an_unknown_reset_token_is_rejected() {
        let (_dir, pool) = temp_pool();
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        let (status, _) = client
            .post(
                "/api/v1/auth/password-reset/redeem",
                serde_json::json!({"token": "not-a-real-token", "new_password": "whatever new password"}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn admin_reset_password_issues_a_redeemable_token_and_writes_an_audit_row() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "admin-reset@example.test",
            "correct horse battery staple",
        );

        admin_reset_password(pool.clone(), "admin-reset@example.test".to_string(), true).unwrap();

        let conn = pool.get().unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM password_reset_tokens", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
        let action: String = conn
            .query_row(
                "SELECT action FROM audit_log WHERE action = 'user.password_reset_issued_via_admin_cli'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(action, "user.password_reset_issued_via_admin_cli");
    }

    #[test]
    fn admin_reset_password_fails_for_an_unknown_email() {
        let (_dir, pool) = temp_pool();
        let result = admin_reset_password(pool, "nobody@example.test".to_string(), true);
        assert!(result.is_err());
    }

    #[test]
    fn admin_disable_2fa_force_clears_an_enabled_credential() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "admin-2fa@example.test",
            "correct horse battery staple",
        );
        let user_id = {
            let conn = pool.get().unwrap();
            domain::users::find_by_email(&conn, "admin-2fa@example.test")
                .unwrap()
                .unwrap()
                .id
        };
        {
            let mut conn = pool.get().unwrap();
            let pending =
                domain::two_factor::setup(&conn, &user_id, "admin-2fa@example.test").unwrap();
            let code = totp_code_for(&pending.secret_base32, "admin-2fa@example.test");
            domain::two_factor::confirm(&mut conn, &user_id, "admin-2fa@example.test", &code)
                .unwrap();
        }
        assert!(
            domain::two_factor::status(&pool.get().unwrap(), &user_id)
                .unwrap()
                .enabled
        );

        admin_disable_2fa(pool.clone(), "admin-2fa@example.test".to_string(), true).unwrap();

        assert!(
            !domain::two_factor::status(&pool.get().unwrap(), &user_id)
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn admin_unlock_account_clears_a_lockout() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "admin-unlock@example.test",
            "correct horse battery staple",
        );
        let conn = pool.get().unwrap();
        conn.execute(
            "UPDATE users SET failed_login_count = 99, locked_until = 9999999999 WHERE email = 'admin-unlock@example.test'",
            [],
        )
        .unwrap();
        drop(conn);

        admin_unlock_account(pool.clone(), "admin-unlock@example.test".to_string(), true).unwrap();

        let conn = pool.get().unwrap();
        let locked_until: Option<i64> = conn
            .query_row(
                "SELECT locked_until FROM users WHERE email = 'admin-unlock@example.test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(locked_until.is_none());
    }

    #[test]
    fn admin_force_end_link_ends_the_link_and_fails_for_an_unknown_id() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "admin-fel@example.test",
            "correct horse battery staple",
        );
        let (keyholder_id, submissive_id) = {
            let conn = pool.get().unwrap();
            let kh = domain::users::find_by_email(&conn, "admin-fel@example.test")
                .unwrap()
                .unwrap()
                .id;
            let sub_id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
                 VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Sub', 0)",
                rusqlite::params![sub_id],
            )
            .unwrap();
            (kh, sub_id)
        };
        let link_id = {
            let conn = pool.get().unwrap();
            domain::links::create(&conn, &keyholder_id, &submissive_id).unwrap()
        };

        admin_force_end_link(pool.clone(), link_id.clone(), true).unwrap();

        let conn = pool.get().unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM keyholder_submissive_links WHERE id = ?1",
                rusqlite::params![link_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "ended");
        drop(conn);

        let result = admin_force_end_link(pool, "not-a-real-link-id".to_string(), true);
        assert!(result.is_err());
    }

    #[test]
    fn admin_backup_copies_the_database_and_blob_directory() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "backup@example.test", "correct horse battery staple");

        let data_dir = tempfile::tempdir().unwrap();
        let blob_dir = data_dir.path().join("blobs");
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::write(blob_dir.join("example.bin"), b"blob contents").unwrap();

        // admin_backup resolves the blob directory via resolve_data_dir(),
        // same as the running server does; the guard restores DATA_DIR on
        // drop (including on panic) so a failing assertion below can't
        // leak process-global env state into later tests.
        struct DataDirGuard;
        impl Drop for DataDirGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("DATA_DIR");
                }
            }
        }
        unsafe {
            std::env::set_var("DATA_DIR", data_dir.path());
        }
        let _guard = DataDirGuard;

        let out_dir = tempfile::tempdir().unwrap();
        admin_backup(pool, out_dir.path().to_path_buf()).unwrap();

        assert!(out_dir.path().join("db.sqlite3").is_file());
        assert!(out_dir.path().join("blobs/example.bin").is_file());
        assert_eq!(
            std::fs::read(out_dir.path().join("blobs/example.bin")).unwrap(),
            b"blob contents"
        );
    }

    #[tokio::test]
    async fn mutating_request_without_csrf_is_rejected() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh6@example.test", "correct horse battery staple");
        let blob_dir = tempfile::tempdir().unwrap();
        let app = test_app_state(
            pool,
            blob_dir.path().to_path_buf(),
            live::PlaySessionStreams::default(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"email": "kh6@example.test", "password": "x"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn login_page_renders() {
        let (_dir, pool) = temp_pool();
        let mut client = TestClient::new(pool);
        let (status, _, body) = client.get_page("/login").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Sign in"));
    }

    #[tokio::test]
    async fn redeem_invite_page_renders() {
        let (_dir, pool) = temp_pool();
        let mut client = TestClient::new(pool);
        let (status, _, body) = client.get_page("/invites/redeem").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Create account"));
    }

    #[tokio::test]
    async fn password_reset_redeem_page_renders() {
        let (_dir, pool) = temp_pool();
        let mut client = TestClient::new(pool);
        let (status, _, body) = client.get_page("/password-reset/redeem").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Set new password"));
    }

    #[tokio::test]
    async fn dashboard_redirects_to_login_when_unauthenticated() {
        let (_dir, pool) = temp_pool();
        let mut client = TestClient::new(pool);
        let (status, location, _) = client.get_page("/dashboard").await;
        assert!(status.is_redirection());
        assert_eq!(location.as_deref(), Some("/login"));
    }

    #[tokio::test]
    async fn index_redirects_by_auth_state() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh7@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool);

        let (status, location, _) = client.get_page("/").await;
        assert!(status.is_redirection());
        assert_eq!(location.as_deref(), Some("/login"));

        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh7@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, location, _) = client.get_page("/").await;
        assert!(status.is_redirection());
        assert_eq!(location.as_deref(), Some("/dashboard"));
    }

    #[tokio::test]
    async fn dashboard_renders_real_roster_data_for_authenticated_keyholder() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh8@example.test", "correct horse battery staple");
        let mut keyholder = TestClient::new(pool.clone());
        keyholder.get("/health").await;
        keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh8@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (_, invite_body) = keyholder
            .post("/api/v1/keyholder/invites", serde_json::json!({}))
            .await;
        let token = invite_body["token"].as_str().unwrap().to_string();

        let mut submissive = TestClient::new(pool.clone());
        submissive.get("/health").await;
        submissive
            .post(
                "/api/v1/auth/invites/redeem",
                serde_json::json!({
                    "token": token,
                    "email": "roster-sub@example.test",
                    "password": "another strong password",
                    "display_name": "Roster Sub",
                }),
            )
            .await;

        let (status, _, body) = keyholder.get_page("/dashboard").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Roster Sub"));
        assert!(body.contains("KH")); // the logged-in keyholder's own display name
    }

    #[tokio::test]
    async fn dashboard_shows_empty_state_with_no_roster() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh9@example.test", "correct horse battery staple");
        let mut client = TestClient::new(pool);
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh9@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, _, body) = client.get_page("/dashboard").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Nobody linked yet"));
    }

    #[tokio::test]
    async fn static_assets_are_served() {
        let (_dir, pool) = temp_pool();
        let mut client = TestClient::new(pool);
        let (status, _, body) = client.get_page("/static/css/app.css").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.is_empty());
    }

    // ---- Phase 2: chastity, verification, and proof-submission flows ----

    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// Logs a keyholder in, invites a submissive, and redeems that invite
    /// — the real HTTP flow (not a DB shortcut) two separate `TestClient`s
    /// need before any Phase 2 endpoint makes sense.
    async fn linked_keyholder_and_submissive(
        pool: &db::Pool,
        keyholder_email: &str,
        submissive_email: &str,
    ) -> (TestClient, TestClient, tempfile::TempDir) {
        let blob_dir = tempfile::tempdir().unwrap();
        let play_session_streams = live::PlaySessionStreams::default();
        seed_keyholder(pool, keyholder_email, "correct horse battery staple");
        let mut keyholder = TestClient::new_with_blob_dir(
            pool.clone(),
            blob_dir.path(),
            play_session_streams.clone(),
        );
        keyholder.get("/health").await;
        keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": keyholder_email, "password": "correct horse battery staple"}),
            )
            .await;

        let (_, invite_body) = keyholder
            .post("/api/v1/keyholder/invites", serde_json::json!({}))
            .await;
        let token = invite_body["token"].as_str().unwrap().to_string();

        let mut submissive =
            TestClient::new_with_blob_dir(pool.clone(), blob_dir.path(), play_session_streams);
        submissive.get("/health").await;
        submissive
            .post(
                "/api/v1/auth/invites/redeem",
                serde_json::json!({
                    "token": token,
                    "email": submissive_email,
                    "password": "another strong password",
                    "display_name": "Sub",
                }),
            )
            .await;

        (keyholder, submissive, blob_dir)
    }

    async fn submissive_id(keyholder: &mut TestClient) -> String {
        let (_, roster) = keyholder.get("/api/v1/keyholder/submissives").await;
        roster[0]["submissive_id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn api_token_create_list_update_revoke_lifecycle() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "tokens1@example.test",
            "correct horse battery staple",
        );
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tokens1@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, created) = client
            .post(
                "/api/v1/keyholder/api-tokens",
                serde_json::json!({"label": "notifier bot", "scopes": ["read:submissives"], "expires_in_days": 30}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let token_id = created["id"].as_str().unwrap().to_string();
        let raw_token = created["token"].as_str().unwrap().to_string();
        assert!(!created["prefix"].as_str().unwrap().is_empty());
        assert!(created["expires_at"].is_string());

        let (status, list) = client.get("/api/v1/keyholder/api-tokens").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["label"], "notifier bot");
        assert!(list[0].get("token").is_none());

        let (status, _) = client
            .patch(
                &format!("/api/v1/keyholder/api-tokens/{token_id}"),
                serde_json::json!({"label": "renamed bot"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = client.get("/api/v1/keyholder/api-tokens").await;
        assert_eq!(list[0]["label"], "renamed bot");

        // The raw token authenticates via Bearer before revocation.
        let (status, _) = client.get_bearer("/api/v1/auth/me", &raw_token).await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = client
            .delete(&format!("/api/v1/keyholder/api-tokens/{token_id}"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Revocation is immediate.
        let (status, _) = client.get_bearer("/api/v1/auth/me", &raw_token).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bearer_token_with_the_right_scope_reaches_a_keyholder_endpoint() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, _submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "tokens2-kh@example.test",
            "tokens2-sub@example.test",
        )
        .await;

        let (_, created) = keyholder
            .post(
                "/api/v1/keyholder/api-tokens",
                serde_json::json!({"label": "roster reader", "scopes": ["read:submissives"]}),
            )
            .await;
        let raw_token = created["token"].as_str().unwrap().to_string();

        let (status, roster) = keyholder
            .get_bearer("/api/v1/keyholder/submissives", &raw_token)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(roster.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn bearer_token_missing_the_needed_scope_is_forbidden_not_unauthorized() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, _submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "tokens3-kh@example.test",
            "tokens3-sub@example.test",
        )
        .await;

        let (_, created) = keyholder
            .post(
                "/api/v1/keyholder/api-tokens",
                serde_json::json!({"label": "roster reader", "scopes": ["read:submissives"]}),
            )
            .await;
        let raw_token = created["token"].as_str().unwrap().to_string();

        // This token has no manage:invites scope.
        let (status, _) = keyholder
            .post_bearer(
                "/api/v1/keyholder/invites",
                &raw_token,
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn bearer_token_requests_bypass_csrf() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, _submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "tokens4-kh@example.test",
            "tokens4-sub@example.test",
        )
        .await;

        let (_, created) = keyholder
            .post(
                "/api/v1/keyholder/api-tokens",
                serde_json::json!({"label": "invite bot", "scopes": ["manage:invites"]}),
            )
            .await;
        let raw_token = created["token"].as_str().unwrap().to_string();

        // No CSRF cookie/header is ever sent on a bearer request.
        let (status, invite) = keyholder
            .post_bearer(
                "/api/v1/keyholder/invites",
                &raw_token,
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(invite["token"].is_string());
    }

    #[tokio::test]
    async fn a_token_with_no_scopes_can_only_introspect_itself() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "tokens5@example.test",
            "correct horse battery staple",
        );
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tokens5@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (_, created) = client
            .post(
                "/api/v1/keyholder/api-tokens",
                serde_json::json!({"label": "bare token"}),
            )
            .await;
        let raw_token = created["token"].as_str().unwrap().to_string();

        let (status, me) = client.get_bearer("/api/v1/auth/me", &raw_token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(me["role"], "keyholder");

        let (status, _) = client
            .get_bearer("/api/v1/keyholder/submissives", &raw_token)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn session_management_endpoints_reject_bearer_token_auth() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "tokens6@example.test",
            "correct horse battery staple",
        );
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "tokens6@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (_, created) = client
            .post(
                "/api/v1/keyholder/api-tokens",
                serde_json::json!({"label": "bot"}),
            )
            .await;
        let raw_token = created["token"].as_str().unwrap().to_string();

        let (status, _) = client.get_bearer("/api/v1/auth/sessions", &raw_token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, _) = client
            .post_bearer(
                "/api/v1/auth/password/change",
                &raw_token,
                serde_json::json!({"current_password": "correct horse battery staple", "new_password": "whatever new password"}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn link_status_only_allows_forward_transitions() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, _submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-linkstatus@example.test",
            "sub-linkstatus@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link"),
                serde_json::json!({"status": "paused"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Can't go back to active — not even a recognized target value.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link"),
                serde_json::json!({"status": "active"}),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link"),
                serde_json::json!({"status": "ended"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Once ended, the link is no longer active/paused, so it's no
        // longer resolvable as "yours to act on" at all (same as every
        // other write-scoped endpoint) — 404, not 409.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link"),
                serde_json::json!({"status": "paused"}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // The ended link no longer shows up in the active roster.
        let (_, roster) = keyholder.get("/api/v1/keyholder/submissives").await;
        assert_eq!(roster.as_array().unwrap().len(), 0);
    }

    /// 06-future-extensions.md §2: self-service link ending, the
    /// request-not-action shape. Covers the full round trip — request,
    /// duplicate rejection, the Keyholder's read side, decline (with
    /// the note reaching the submissive, and the link staying active),
    /// re-request after a decline, withdrawal, and finally a request
    /// that's actually accepted (ending the link and clearing the
    /// request fields as a side effect).
    #[tokio::test]
    async fn link_end_request_lifecycle_request_decline_withdraw_and_accept() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-endreq@example.test",
            "sub-endreq@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        // Requesting works and notifies the Keyholder.
        let (status, _) = submissive
            .post(
                "/api/v1/submissive/link/end-request",
                serde_json::json!({"reason": "need some space"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, kh_feed) = keyholder.get("/api/v1/notifications").await;
        assert!(
            kh_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "link.end_requested")
        );

        // A second request while one's pending is rejected.
        let (status, _) = submissive
            .post("/api/v1/submissive/link/end-request", serde_json::json!({}))
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // The Keyholder can see it.
        let (status, pending) = keyholder.get("/api/v1/keyholder/link-end-requests").await;
        assert_eq!(status, StatusCode::OK);
        let pending = pending.as_array().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["submissive_id"], sub_id);
        assert_eq!(pending[0]["reason"], "need some space");
        assert!(pending[0]["escalated_at"].is_null());

        // Declining clears it, delivers the note, and leaves the link active.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link/end-request/decline"),
                serde_json::json!({"response_note": "let's talk first"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, pending) = keyholder.get("/api/v1/keyholder/link-end-requests").await;
        assert_eq!(pending.as_array().unwrap().len(), 0);
        let (_, sub_feed) = submissive.get("/api/v1/notifications").await;
        let declined = sub_feed
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["type"] == "link.end_request_declined")
            .expect("submissive was notified of the decline");
        assert_eq!(declined["body"], "let's talk first");
        let (_, roster) = keyholder.get("/api/v1/keyholder/submissives").await;
        assert_eq!(roster.as_array().unwrap().len(), 1);

        // Declining isn't final — a fresh request reopens it.
        let (status, _) = submissive
            .post("/api/v1/submissive/link/end-request", serde_json::json!({}))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, pending) = keyholder.get("/api/v1/keyholder/link-end-requests").await;
        assert_eq!(pending.as_array().unwrap().len(), 1);

        // The submissive can withdraw their own request at any time.
        let (status, _) = submissive
            .request("DELETE", "/api/v1/submissive/link/end-request", None)
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, pending) = keyholder.get("/api/v1/keyholder/link-end-requests").await;
        assert_eq!(pending.as_array().unwrap().len(), 0);
        // Withdrawing when nothing's pending is a harmless no-op.
        let (status, _) = submissive
            .request("DELETE", "/api/v1/submissive/link/end-request", None)
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Accepting: the existing PATCH .../link {status:"ended"} route
        // is the acceptance path — no separate "approve" endpoint.
        submissive
            .post("/api/v1/submissive/link/end-request", serde_json::json!({}))
            .await;
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link"),
                serde_json::json!({"status": "ended"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, roster) = keyholder.get("/api/v1/keyholder/submissives").await;
        assert_eq!(roster.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn own_profile_round_trips_role_appropriate_fields() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "kh-profile@example.test",
            "correct horse battery staple",
        );
        let mut client = TestClient::new(pool.clone());
        client.get("/health").await;
        client
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-profile@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, profile) = client.get("/api/v1/profile").await;
        assert_eq!(status, StatusCode::OK);
        assert!(profile["bio"].is_null());
        assert!(profile.get("safeword").is_none());

        let (status, _) = client
            .patch(
                "/api/v1/profile",
                serde_json::json!({"bio": "A strict but fair keyholder.", "contact_info": "signal: kh123"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, profile) = client.get("/api/v1/profile").await;
        assert_eq!(profile["bio"], "A strict but fair keyholder.");
        assert_eq!(profile["contact_info"], "signal: kh123");

        // Clearing a field explicitly.
        client
            .patch("/api/v1/profile", serde_json::json!({"bio": null}))
            .await;
        let (_, profile) = client.get("/api/v1/profile").await;
        assert!(profile["bio"].is_null());
        assert_eq!(profile["contact_info"], "signal: kh123");
    }

    #[tokio::test]
    async fn display_name_is_editable_for_both_roles_and_rejects_empty() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-name@example.test", "sub-name@example.test")
                .await;

        let (_, profile) = keyholder.get("/api/v1/profile").await;
        assert_eq!(profile["display_name"], "KH");
        let (status, _) = keyholder
            .patch(
                "/api/v1/profile",
                serde_json::json!({"display_name": "Alex"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, profile) = keyholder.get("/api/v1/profile").await;
        assert_eq!(profile["display_name"], "Alex");

        let (_, profile) = submissive.get("/api/v1/profile").await;
        assert_eq!(profile["display_name"], "Sub");
        let (status, _) = submissive
            .patch(
                "/api/v1/profile",
                serde_json::json!({"display_name": "Riley"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, profile) = submissive.get("/api/v1/profile").await;
        assert_eq!(profile["display_name"], "Riley");

        // Empty (or whitespace-only) is rejected, not silently accepted.
        let (status, _) = submissive
            .patch(
                "/api/v1/profile",
                serde_json::json!({"display_name": "   "}),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (_, profile) = submissive.get("/api/v1/profile").await;
        assert_eq!(profile["display_name"], "Riley");

        // The new name shows up fresh on a brand new login too, not just
        // the request that changed it — confirming it's read live from
        // `users`, not cached in the session.
        let (status, login_body) = submissive
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "sub-name@example.test", "password": "another strong password"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(login_body["display_name"], "Riley");
    }

    #[tokio::test]
    async fn keyholder_notes_are_keyholder_only_and_hidden_from_the_submissives_own_profile() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-notes@example.test",
            "sub-notes@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        submissive
            .patch(
                "/api/v1/profile",
                serde_json::json!({"bio": "here to behave", "safeword": "banana"}),
            )
            .await;

        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/profile/notes"),
                serde_json::json!({"keyholder_notes": "keeps missing verification windows"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // The keyholder sees everything, including their own notes.
        let (status, profile) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/profile"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(profile["bio"], "here to behave");
        assert_eq!(profile["safeword"], "banana");
        assert_eq!(
            profile["keyholder_notes"],
            "keeps missing verification windows"
        );

        // The submissive's own profile fetch never includes the notes
        // field at all, and can't write it either.
        let (_, own_profile) = submissive.get("/api/v1/profile").await;
        assert!(own_profile.get("keyholder_notes").is_none());

        submissive
            .patch(
                "/api/v1/profile",
                serde_json::json!({"keyholder_notes": "sneaky"}),
            )
            .await;
        let (_, profile) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/profile"))
            .await;
        assert_eq!(
            profile["keyholder_notes"],
            "keeps missing verification windows"
        );

        // A different keyholder can't read or write this submissive's profile at all.
        seed_keyholder(
            &pool,
            "kh-notes-other@example.test",
            "correct horse battery staple",
        );
        let mut other = TestClient::new(pool.clone());
        other.get("/health").await;
        other
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-notes-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/profile"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn submissive_can_read_their_keyholders_boundaries_but_not_write_them() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-boundaries@example.test",
            "sub-boundaries@example.test",
        )
        .await;

        keyholder
            .patch(
                "/api/v1/profile",
                serde_json::json!({"bio": "Firm but fair.", "hard_limits": "no permanent marks", "soft_limits": "ask first", "okay_limits": "impact play"}),
            )
            .await;

        let (status, kh_profile) = submissive.get("/api/v1/submissive/keyholder-profile").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(kh_profile["bio"], "Firm but fair.");
        assert_eq!(kh_profile["hard_limits"], "no permanent marks");
        assert_eq!(kh_profile["soft_limits"], "ask first");
        assert_eq!(kh_profile["okay_limits"], "impact play");
        assert!(kh_profile.get("contact_info").is_some());

        // A keyholder has no linked keyholder of their own to fetch.
        let (status, _) = keyholder.get("/api/v1/submissive/keyholder-profile").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn link_settings_gate_submissive_self_report() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-selfreport@example.test",
            "sub-selfreport@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;
        let (_, device) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/devices"),
                serde_json::json!({"name": "steel #1"}),
            )
            .await;
        let device_id = device["id"].as_str().unwrap().to_string();

        // Off by default.
        let (status, _) = submissive
            .post(
                "/api/v1/submissive/confinement-sessions",
                serde_json::json!({"device_id": device_id, "started_reason": "voluntary"}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // A submissive can't grant themselves the setting.
        let (status, _) = submissive
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link/settings"),
                serde_json::json!({"self_report_allowed": true, "catalog_visible_to_submissive": true}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link/settings"),
                serde_json::json!({"self_report_allowed": true, "catalog_visible_to_submissive": true}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = submissive
            .post(
                "/api/v1/submissive/confinement-sessions",
                serde_json::json!({"device_id": device_id, "started_reason": "voluntary"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let (_, status_body) = submissive.get("/api/v1/submissive/status").await;
        assert_eq!(status_body["locked"], true);
        let session_id = status_body["session_id"].as_str().unwrap().to_string();

        let (status, _) = submissive
            .patch(
                &format!("/api/v1/submissive/confinement-sessions/{session_id}"),
                serde_json::json!({"ended_reason": "scheduled_release"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, status_body) = submissive.get("/api/v1/submissive/status").await;
        assert_eq!(status_body["locked"], false);
    }

    #[tokio::test]
    async fn device_and_confinement_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-device@example.test",
            "sub-device@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, device) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/devices"),
                serde_json::json!({"name": "steel #2", "description": "daily wear"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let device_id = device["id"].as_str().unwrap().to_string();

        let (status, status_body) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/status"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(status_body["locked"], false);

        let (status, status_body) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/confinement-sessions"),
                serde_json::json!({"device_id": device_id, "started_reason": "voluntary"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(status_body["locked"], true);
        let session_id = status_body["session_id"].as_str().unwrap().to_string();

        // A second open session is rejected.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/confinement-sessions"),
                serde_json::json!({"device_id": device_id, "started_reason": "voluntary"}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, _) = keyholder
            .post(
                &format!(
                    "/api/v1/keyholder/submissives/{sub_id}/confinement-sessions/{session_id}/pause"
                ),
                serde_json::json!({"message": "traveling"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, status_body) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/status"))
            .await;
        assert_eq!(status_body["clock_paused"], true);
        assert_eq!(status_body["clock_pause_message"], "traveling");

        let (_, feed) = submissive.get("/api/v1/notifications").await;
        assert!(
            feed.as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "confinement.clocks_paused")
        );

        let (status, _) = keyholder
            .post(
                &format!(
                    "/api/v1/keyholder/submissives/{sub_id}/confinement-sessions/{session_id}/resume"
                ),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, status_body) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/status"))
            .await;
        assert_eq!(status_body["clock_paused"], false);

        let (_, feed) = submissive.get("/api/v1/notifications").await;
        assert!(
            feed.as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "confinement.clocks_resumed")
        );

        let (status, _) = keyholder
            .patch(
                &format!(
                    "/api/v1/keyholder/submissives/{sub_id}/confinement-sessions/{session_id}"
                ),
                serde_json::json!({"ended_reason": "scheduled_release"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, status_body) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/status"))
            .await;
        assert_eq!(status_body["locked"], false);
    }

    #[tokio::test]
    async fn unrelated_keyholder_gets_404_not_500_touching_someone_elses_proof_or_assignment() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-ownercheck@example.test",
            "sub-ownercheck@example.test",
        )
        .await;

        submissive
            .post_multipart(
                "/api/v1/submissive/proof-submissions",
                &[("kind", "note")],
                &[],
            )
            .await;
        let (_, submissions) = keyholder.get("/api/v1/keyholder/proof-submissions").await;
        let submission_id = submissions[0]["id"].as_str().unwrap().to_string();

        let (_, assignment) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({"kind": "reward", "title": "movie night", "effect_kind": "grant"}),
            )
            .await;
        let sub_id = submissive_id(&mut keyholder).await;
        let (_, assignment) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/assignments"),
                serde_json::json!({"kind": "reward", "template_id": assignment["id"]}),
            )
            .await;
        let assignment_id = assignment["id"].as_str().unwrap().to_string();

        seed_keyholder(
            &pool,
            "kh-ownercheck-other@example.test",
            "correct horse battery staple",
        );
        let mut other = TestClient::new(pool.clone());
        other.get("/health").await;
        other
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-ownercheck-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, _) = other
            .get(&format!(
                "/api/v1/keyholder/proof-submissions/{submission_id}"
            ))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = other
            .get(&format!("/api/v1/keyholder/assignments/{assignment_id}"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn verification_and_proof_review_flow() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-verify@example.test",
            "sub-verify@example.test",
        )
        .await;

        let (status, code_body) = submissive
            .post(
                "/api/v1/submissive/verification-codes",
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let code_id_check = submissive
            .get("/api/v1/submissive/verification-codes/current")
            .await;
        assert_eq!(code_id_check.0, StatusCode::OK);
        assert_eq!(code_id_check.1["code"], code_body["code"]);

        // A second on-demand request while one is still live is a conflict.
        let (status, _) = submissive
            .post(
                "/api/v1/submissive/verification-codes",
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // The proof-submission API needs the code's id, not its display
        // value — fetch it via the keyholder's issued-code history, the
        // only place the raw id is exposed today (deliberately: the
        // *code text* is what the submissive types, not an id).
        let sub_id = submissive_id(&mut keyholder).await;
        // Not needed for submission (client only ever sends the code's
        // own id from /current in a real UI); simulate that by resolving
        // it straight from the DB in this test.
        let conn = pool.get().unwrap();
        let code_id: String = conn
            .query_row(
                "SELECT id FROM verification_codes WHERE code = ?1",
                rusqlite::params![code_body["code"].as_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        let (status, submission) = submissive
            .post_multipart(
                "/api/v1/submissive/proof-submissions",
                &[("verification_code_id", &code_id), ("kind", "photo")],
                &[("files", "proof.png", "image/png", TINY_PNG)],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(submission["status"], "pending");
        assert_eq!(
            submission["verification_code_value"],
            code_body["code"].clone()
        );
        assert_eq!(submission["attachments"].as_array().unwrap().len(), 1);
        let submission_id = submission["id"].as_str().unwrap().to_string();

        // The code is now consumed — no more "current" code.
        let current = submissive
            .get("/api/v1/submissive/verification-codes/current")
            .await;
        assert!(current.1.is_null());

        // Cross-roster feed shows it.
        let (status, feed) = keyholder.get("/api/v1/keyholder/proof-submissions").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(feed.as_array().unwrap().len(), 1);

        // Download the attachment as the keyholder.
        let attachment_id = submission["attachments"][0]["id"].as_str().unwrap();
        let (dl_status, _, dl_body) = keyholder
            .get_page(&format!(
                "/api/v1/keyholder/proof-submissions/{submission_id}/attachments/{attachment_id}"
            ))
            .await;
        assert_eq!(dl_status, StatusCode::OK);
        assert!(!dl_body.is_empty());

        // Review it as redo.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/proof-submissions/{submission_id}/review"),
                serde_json::json!({"status": "redo", "review_notes": "try again"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // A second review of the same submission is rejected.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/proof-submissions/{submission_id}/review"),
                serde_json::json!({"status": "verified"}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // Resubmit as a redo, then verify it.
        let (status, redo_submission) = submissive
            .post_multipart(
                "/api/v1/submissive/proof-submissions",
                &[("kind", "photo"), ("redo_of_submission_id", &submission_id)],
                &[("files", "proof2.png", "image/png", TINY_PNG)],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let redo_id = redo_submission["id"].as_str().unwrap().to_string();

        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/proof-submissions/{redo_id}/review"),
                serde_json::json!({"status": "verified"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, own_list) = submissive.get("/api/v1/submissive/proof-submissions").await;
        let verified = own_list
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == redo_id)
            .unwrap();
        assert_eq!(verified["status"], "verified");
        assert_eq!(verified["reviewed_via"], "session");
        let _ = sub_id; // scoping-only; already exercised above
    }

    #[tokio::test]
    async fn review_queue_and_per_submissive_page_attribute_cards_to_a_submissive() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-attr@example.test", "sub-attr@example.test")
                .await;
        let sub_id = submissive_id(&mut keyholder).await;

        submissive
            .post_multipart(
                "/api/v1/submissive/proof-submissions",
                &[("kind", "note")],
                &[],
            )
            .await;

        let (status, _, queue_body) = keyholder.get_page("/keyholder/review").await;
        assert_eq!(status, StatusCode::OK);
        assert!(queue_body.contains("Sub"));

        let (status, _, per_sub_body) = keyholder
            .get_page(&format!("/keyholder/submissives/{sub_id}/review"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(per_sub_body.contains("Sub"));
    }

    #[tokio::test]
    async fn failing_a_review_can_attach_an_ad_hoc_or_catalog_punishment() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-attach@example.test",
            "sub-attach@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/devices"),
                serde_json::json!({"name": "steel #1"}),
            )
            .await;
        let (_, devices) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/devices"))
            .await;
        let device_id = devices[0]["id"].as_str().unwrap();
        let (_, status_before) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/confinement-sessions"),
                serde_json::json!({"device_id": device_id, "started_reason": "voluntary", "target_release_at": 1_000_000}),
            )
            .await;
        assert_eq!(status_before["locked"], true);

        // Ad-hoc punishment, the "Create new" tab not saved to catalog.
        let (_, submission1) = submissive
            .post_multipart(
                "/api/v1/submissive/proof-submissions",
                &[("kind", "note")],
                &[],
            )
            .await;
        let id1 = submission1["id"].as_str().unwrap();
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/proof-submissions/{id1}/review"),
                serde_json::json!({"status": "failed", "review_notes": null}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/assignments"),
                serde_json::json!({
                    "kind": "punishment", "title": "ad-hoc extra day",
                    "effect_kind": "time_extension", "time_extension_seconds": 86400,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (_, status_after) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/status"))
            .await;
        assert_eq!(
            status_after["target_release_at"],
            api::iso8601(1_000_000 + 86400)
        );

        // From-catalog punishment, the "From catalog" tab.
        let (_, template) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({
                    "kind": "punishment", "title": "catalog extra day",
                    "effect_kind": "time_extension", "time_extension_seconds": 43200,
                }),
            )
            .await;
        let template_id = template["id"].as_str().unwrap();
        let (_, submission2) = submissive
            .post_multipart(
                "/api/v1/submissive/proof-submissions",
                &[("kind", "note")],
                &[],
            )
            .await;
        let id2 = submission2["id"].as_str().unwrap();
        keyholder
            .post(
                &format!("/api/v1/keyholder/proof-submissions/{id2}/review"),
                serde_json::json!({"status": "failed", "review_notes": null}),
            )
            .await;
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/assignments"),
                serde_json::json!({"template_id": template_id}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (_, status_final) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/status"))
            .await;
        assert_eq!(
            status_final["target_release_at"],
            api::iso8601(1_000_000 + 86400 + 43200)
        );
        let _ = blob_dir;
    }

    #[tokio::test]
    async fn submissive_cannot_review_or_touch_another_submissives_devices() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-acl@example.test", "sub-acl@example.test")
                .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, _) = submissive
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/devices"),
                serde_json::json!({"name": "not allowed"}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // A second, unrelated keyholder can't see this submissive at all.
        seed_keyholder(
            &pool,
            "kh-other@example.test",
            "correct horse battery staple",
        );
        let mut other_keyholder = TestClient::new(pool.clone());
        other_keyholder.get("/health").await;
        other_keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _, _) = other_keyholder
            .get_page(&format!("/api/v1/keyholder/submissives/{sub_id}/status"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn safety_alert_lifecycle_raise_list_acknowledge_resolve() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-safety@example.test",
            "sub-safety@example.test",
        )
        .await;

        // Reachable by the submissive with just a message, no other setup.
        let (status, _) = submissive
            .post(
                "/api/v1/submissive/safety-alert",
                serde_json::json!({"message": "device feels too tight"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Raising an alert also lands a push-worthy notification in the
        // keyholder's feed (09-notifications.md §3) — alongside the
        // `link.established` one from redeeming the invite that set up
        // this test's roster, so look up by type rather than position
        // (both can land within the same one-second timestamp).
        let (_, feed) = keyholder.get("/api/v1/notifications").await;
        let notifications = feed.as_array().unwrap();
        assert!(
            notifications
                .iter()
                .any(|n| n["type"] == "safety.alert_raised")
        );

        // A submissive can't read the keyholder-side list.
        let (status, _) = submissive.get("/api/v1/keyholder/safety-alerts").await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, list) = keyholder.get("/api/v1/keyholder/safety-alerts").await;
        assert_eq!(status, StatusCode::OK);
        let alerts = list.as_array().unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["message"], "device feels too tight");
        assert_eq!(alerts[0]["raised_via"], "submissive");
        assert_eq!(alerts[0]["submissive_display_name"], "Sub");
        assert!(alerts[0]["acknowledged_at"].is_null());
        let alert_id = alerts[0]["id"].as_str().unwrap().to_string();

        // A second, unrelated keyholder can't act on it.
        seed_keyholder(
            &pool,
            "kh-safety-other@example.test",
            "correct horse battery staple",
        );
        let mut other_keyholder = TestClient::new(pool.clone());
        other_keyholder.get("/health").await;
        other_keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-safety-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other_keyholder
            .patch(
                &format!("/api/v1/keyholder/safety-alerts/{alert_id}"),
                serde_json::json!({"acknowledged": true}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/safety-alerts/{alert_id}"),
                serde_json::json!({"acknowledged": true}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, list) = keyholder.get("/api/v1/keyholder/safety-alerts").await;
        assert!(!list[0]["acknowledged_at"].is_null());
        assert!(list[0]["resolved_at"].is_null());

        // Acknowledging notifies the submissive back.
        let (_, feed) = submissive.get("/api/v1/notifications").await;
        let notifications = feed.as_array().unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0]["type"], "safety.acknowledged");

        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/safety-alerts/{alert_id}"),
                serde_json::json!({"resolved": true}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = keyholder.get("/api/v1/keyholder/safety-alerts").await;
        assert!(!list[0]["resolved_at"].is_null());
    }

    #[tokio::test]
    async fn vapid_public_key_is_stable_across_requests() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "kh-vapid@example.test",
            "correct horse battery staple",
        );
        let mut keyholder = TestClient::new(pool.clone());
        keyholder.get("/health").await;
        keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-vapid@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, first) = keyholder
            .get("/api/v1/notifications/vapid-public-key")
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!first["public_key"].as_str().unwrap().is_empty());

        let (_, second) = keyholder
            .get("/api/v1/notifications/vapid-public-key")
            .await;
        assert_eq!(first["public_key"], second["public_key"]);
    }

    #[tokio::test]
    async fn notification_feed_lists_marks_read_and_is_scoped_per_user() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-notif@example.test",
            "sub-notif@example.test",
        )
        .await;

        let kh_id: String = {
            let conn = pool.get().unwrap();
            conn.query_row(
                "SELECT id FROM users WHERE email = 'kh-notif@example.test'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        {
            let conn = pool.get().unwrap();
            domain::notifications::create(
                &conn,
                domain::notifications::NewNotification {
                    user_id: &kh_id,
                    link_id: None,
                    notification_type: "safety.alert_raised",
                    title: "Safety alert",
                    body: Some("device feels too tight"),
                    link_path: Some("/keyholder/safety-alerts"),
                    related_entity_type: None,
                    related_entity_id: None,
                },
            )
            .unwrap();
        }

        // Scoped per-user: the submissive's own feed is empty.
        let (status, list) = submissive.get("/api/v1/notifications").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 0);

        // Alongside the `link.established` notification from redeeming
        // the invite that set up this test's roster — look up by type
        // rather than position, since both can land within the same
        // one-second timestamp.
        let (status, list) = keyholder.get("/api/v1/notifications").await;
        assert_eq!(status, StatusCode::OK);
        let notifications = list.as_array().unwrap();
        let alert_notification = notifications
            .iter()
            .find(|n| n["type"] == "safety.alert_raised")
            .expect("safety.alert_raised notification");
        assert!(alert_notification["read_at"].is_null());
        let id = alert_notification["id"].as_str().unwrap().to_string();

        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/notifications/{id}/read"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // The `link.established` notification from setup is still
        // unread — only the one just marked read drops out.
        let (_, list) = keyholder.get("/api/v1/notifications?unread=true").await;
        let unread = list.as_array().unwrap();
        assert!(unread.iter().all(|n| n["type"] != "safety.alert_raised"));

        // A second, unrelated keyholder can't mark someone else's read.
        seed_keyholder(
            &pool,
            "kh-notif-other@example.test",
            "correct horse battery staple",
        );
        let mut other_keyholder = TestClient::new(pool.clone());
        other_keyholder.get("/health").await;
        other_keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-notif-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other_keyholder
            .patch(
                &format!("/api/v1/notifications/{id}/read"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = keyholder
            .patch("/api/v1/notifications/read-all", serde_json::json!({}))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn push_subscription_lifecycle_register_list_delete() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(
            &pool,
            "kh-push@example.test",
            "correct horse battery staple",
        );
        let mut keyholder = TestClient::new(pool.clone());
        keyholder.get("/health").await;
        keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-push@example.test", "password": "correct horse battery staple"}),
            )
            .await;

        let (status, created) = keyholder
            .post(
                "/api/v1/notifications/push-subscriptions",
                serde_json::json!({
                    "endpoint": "https://push.example/ep-1",
                    "keys": {"p256dh": "p256dh-val", "auth": "auth-val"},
                    "user_agent": "TestBrowser"
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let id = created["id"].as_str().unwrap().to_string();

        let (status, list) = keyholder
            .get("/api/v1/notifications/push-subscriptions")
            .await;
        assert_eq!(status, StatusCode::OK);
        let subs = list.as_array().unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0]["user_agent"], "TestBrowser");
        assert!(subs[0].get("p256dh").is_none());

        // Re-registering the same endpoint updates rather than duplicating.
        let (status, _) = keyholder
            .post(
                "/api/v1/notifications/push-subscriptions",
                serde_json::json!({
                    "endpoint": "https://push.example/ep-1",
                    "keys": {"p256dh": "p256dh-val2", "auth": "auth-val2"}
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (_, list) = keyholder
            .get("/api/v1/notifications/push-subscriptions")
            .await;
        assert_eq!(list.as_array().unwrap().len(), 1);

        let (status, _) = keyholder
            .delete(&format!("/api/v1/notifications/push-subscriptions/{id}"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = keyholder
            .get("/api/v1/notifications/push-subscriptions")
            .await;
        assert_eq!(list.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn submissive_dashboard_and_submit_proof_pages_render() {
        let (_dir, pool) = temp_pool();
        let (_keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-pages@example.test",
            "sub-pages@example.test",
        )
        .await;

        let (status, _, body) = submissive.get_page("/submissive").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Unlocked"));

        let (status, _, body) = submissive.get_page("/submissive/submit-proof").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Submit proof"));
    }

    #[tokio::test]
    async fn catalog_page_renders_description_and_badges_for_real_templates() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, _submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-catpage@example.test",
            "sub-catpage@example.test",
        )
        .await;

        let (_, reward) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({
                    "kind": "reward",
                    "title": "Movie night",
                    "effect_kind": "grant"
                }),
            )
            .await;
        let reward_id = reward["id"].as_str().unwrap();

        keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({
                    "kind": "task",
                    "title": "Evening check-in",
                    "description": "Photo proof every night",
                    "completion_type": "proof_required",
                    "proof_media_types": ["photo"],
                    "default_deadline_seconds": 43200,
                    "on_success_template_id": reward_id
                }),
            )
            .await;

        keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({
                    "kind": "punishment",
                    "title": "Extra day locked",
                    "effect_kind": "time_extension",
                    "time_extension_seconds": 86400
                }),
            )
            .await;

        let (status, _, body) = keyholder.get_page("/keyholder/catalog").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Photo proof every night"));
        assert!(body.contains("Proof required"));
        assert!(body.contains("succeeds into: Movie night"));
        assert!(body.contains("extends lock timer by 1d"));
        assert!(body.contains("data-kind=\"punishment\""));
        assert!(body.contains("data-kind=\"reward\""));
    }

    #[tokio::test]
    async fn keyholder_pages_render_submissive_detail_and_review_queue() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-pages2@example.test",
            "sub-pages2@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, _, body) = keyholder
            .get_page(&format!("/keyholder/submissives/{sub_id}"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Devices"));

        let (status, _, body) = keyholder.get_page("/keyholder/review").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Nothing waiting on review"));

        // Submit something and confirm it now shows up in the queue.
        submissive
            .post_multipart(
                "/api/v1/submissive/proof-submissions",
                &[("kind", "note")],
                &[],
            )
            .await;
        let (status, _, body) = keyholder.get_page("/keyholder/review").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("note"));
        assert!(!body.contains("Nothing waiting on review"));
    }

    #[tokio::test]
    async fn safety_alerts_page_renders_for_keyholder_and_redirects_submissive() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-safetypage@example.test",
            "sub-safetypage@example.test",
        )
        .await;

        let (status, _, body) = keyholder.get_page("/keyholder/safety-alerts").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Safety Alerts"));

        let (status, location, _) = submissive.get_page("/keyholder/safety-alerts").await;
        assert!(status.is_redirection());
        assert_eq!(location.as_deref(), Some("/submissive"));
    }

    /// docs/16-mockup-implementation-gaps.md item 5: the per-submissive
    /// review view, additive to the cross-submissive Review Queue —
    /// same underlying pending-proof data, scoped to one link.
    #[tokio::test]
    async fn submissive_review_page_shows_only_that_submissives_pending_proof() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-subreview@example.test",
            "sub-subreview@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, _, body) = keyholder
            .get_page(&format!("/keyholder/submissives/{sub_id}/review"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Nothing waiting on review"));

        submissive
            .post_multipart(
                "/api/v1/submissive/proof-submissions",
                &[("kind", "note")],
                &[],
            )
            .await;

        let (status, _, body) = keyholder
            .get_page(&format!("/keyholder/submissives/{sub_id}/review"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("note"));

        // An unrelated Keyholder can't see this page at all.
        seed_keyholder(
            &pool,
            "kh-subreview-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-subreview-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _, _) = other_kh
            .get_page(&format!("/keyholder/submissives/{sub_id}/review"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn account_settings_page_renders_for_both_roles() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-account@example.test",
            "sub-account@example.test",
        )
        .await;

        let (status, _, body) = keyholder.get_page("/account").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Account & security"));
        assert!(body.contains("API tokens"));

        let (status, _, body) = submissive.get_page("/account").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Account & security"));
        assert!(!body.contains("API tokens"));
    }

    #[tokio::test]
    async fn account_settings_page_redirects_to_login_when_unauthenticated() {
        let (_dir, pool) = temp_pool();
        let mut client = TestClient::new(pool);
        let (status, location, _) = client.get_page("/account").await;
        assert!(status.is_redirection());
        assert_eq!(location.as_deref(), Some("/login"));
    }

    #[tokio::test]
    async fn dashboard_roster_row_links_to_submissive_detail() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, _submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-link@example.test", "sub-link@example.test")
                .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, _, body) = keyholder.get_page("/dashboard").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&format!("/keyholder/submissives/{sub_id}")));
    }

    #[tokio::test]
    async fn dashboard_shows_needs_attention_feed_and_stat_counts() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-attention@example.test",
            "sub-attention@example.test",
        )
        .await;

        submissive
            .post(
                "/api/v1/submissive/safety-alert",
                serde_json::json!({"message": "device feels too tight"}),
            )
            .await;
        submissive
            .post_multipart(
                "/api/v1/submissive/proof-submissions",
                &[("kind", "note")],
                &[],
            )
            .await;

        let (status, _, body) = keyholder.get_page("/dashboard").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Needs your attention"));
        assert!(body.contains("Safety alert from Sub"));
        assert!(body.contains("device feels too tight"));
        assert!(body.contains("Proof submitted, awaiting review"));
        // Stat cards: 1 active submissive, 1 pending review.
        assert!(body.contains("Active submissives"));
        assert!(body.contains("Pending review"));
    }

    // ---- Phase 3: tasks, rewards, punishments, escalation ----

    #[tokio::test]
    async fn catalog_crud_and_access_control() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-cat@example.test", "sub-cat@example.test")
                .await;

        let (status, template) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({
                    "kind": "task",
                    "title": "cold shower",
                    "completion_type": "proof_required",
                    "proof_media_types": ["video"],
                    "default_deadline_seconds": 86400
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(template["kind"], "task");

        // A submissive can't touch the catalog at all.
        let (status, _) = submissive
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({"kind": "task", "title": "x", "completion_type": "acknowledge_only", "default_deadline_seconds": 60}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Missing required combination -> 422.
        let (status, _) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({"kind": "task", "title": "x"}),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let (status, list) = keyholder.get("/api/v1/keyholder/templates").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);

        let template_id = template["id"].as_str().unwrap();
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/templates/{template_id}"),
                serde_json::json!({"active": false}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn submissive_catalog_read_respects_visibility_and_excludes_inactive() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-cat-vis@example.test",
            "sub-cat-vis@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (_, active_task) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({"kind": "task", "title": "visible task", "completion_type": "acknowledge_only", "default_deadline_seconds": 3600}),
            )
            .await;
        let active_id = active_task["id"].as_str().unwrap().to_string();

        let (_, inactive_task) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({"kind": "task", "title": "hidden task", "completion_type": "acknowledge_only", "default_deadline_seconds": 3600}),
            )
            .await;
        let inactive_id = inactive_task["id"].as_str().unwrap().to_string();
        keyholder
            .patch(
                &format!("/api/v1/keyholder/templates/{inactive_id}"),
                serde_json::json!({"active": false}),
            )
            .await;

        // Visible by default (catalog_visible_to_submissive defaults on).
        let (status, list) = submissive.get("/api/v1/submissive/templates").await;
        assert_eq!(status, StatusCode::OK);
        let ids: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&active_id.as_str()));
        assert!(!ids.contains(&inactive_id.as_str())); // inactive excluded

        // Turn visibility off.
        keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link/settings"),
                serde_json::json!({"self_report_allowed": false, "catalog_visible_to_submissive": false, "points_enabled": false}),
            )
            .await;
        let (_, list) = submissive.get("/api/v1/submissive/templates").await;
        assert_eq!(list.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn catalog_template_can_be_reactivated_edited_and_have_its_escalation_cleared() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, _submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-edit@example.test", "sub-edit@example.test")
                .await;

        let (_, fallback) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({"kind": "punishment", "title": "extra day locked", "effect_kind": "grant"}),
            )
            .await;
        let fallback_id = fallback["id"].as_str().unwrap().to_string();

        let (_, task) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({
                    "kind": "task",
                    "title": "cold shower",
                    "completion_type": "acknowledge_only",
                    "default_deadline_seconds": 3600,
                    "on_failure_template_id": fallback_id,
                }),
            )
            .await;
        let task_id = task["id"].as_str().unwrap().to_string();
        assert_eq!(task["on_failure_template_id"], fallback_id);

        // Deactivate, then reactivate via a partial PATCH that only
        // touches `active` — every other field must survive untouched.
        keyholder
            .patch(
                &format!("/api/v1/keyholder/templates/{task_id}"),
                serde_json::json!({"active": false}),
            )
            .await;
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/templates/{task_id}"),
                serde_json::json!({"active": true}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = keyholder.get("/api/v1/keyholder/templates").await;
        let reactivated = list
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == task_id)
            .unwrap();
        assert_eq!(reactivated["active"], true);
        assert_eq!(reactivated["title"], "cold shower");
        assert_eq!(reactivated["default_deadline_seconds"], 3600);
        assert_eq!(reactivated["on_failure_template_id"], fallback_id);

        // Full field edit: rename, and re-deadline.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/templates/{task_id}"),
                serde_json::json!({"title": "cold shower, extended", "default_deadline_seconds": 7200}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = keyholder.get("/api/v1/keyholder/templates").await;
        let edited = list
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == task_id)
            .unwrap();
        assert_eq!(edited["title"], "cold shower, extended");
        assert_eq!(edited["default_deadline_seconds"], 7200);

        // Explicit null clears the escalation chain without touching
        // anything else.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/templates/{task_id}"),
                serde_json::json!({"on_failure_template_id": null}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = keyholder.get("/api/v1/keyholder/templates").await;
        let cleared = list
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == task_id)
            .unwrap();
        assert_eq!(cleared["on_failure_template_id"], serde_json::Value::Null);
        assert_eq!(cleared["title"], "cold shower, extended");

        // An edit that would leave the template in an invalid state is
        // rejected with 422, and doesn't partially apply.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/templates/{fallback_id}"),
                serde_json::json!({"effect_kind": "time_extension"}),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (_, list) = keyholder.get("/api/v1/keyholder/templates").await;
        let untouched = list
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == fallback_id)
            .unwrap();
        assert_eq!(untouched["effect_kind"], "grant");
    }

    #[tokio::test]
    async fn task_assignment_failure_escalates_to_a_time_extension_punishment() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-task@example.test", "sub-task@example.test")
                .await;
        let sub_id = submissive_id(&mut keyholder).await;

        // Lock the submissive so there's a confinement timer to extend.
        let (_, device) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/devices"),
                serde_json::json!({"name": "cage"}),
            )
            .await;
        let device_id = device["id"].as_str().unwrap();
        keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/confinement-sessions"),
                serde_json::json!({"device_id": device_id, "started_reason": "voluntary", "target_release_at": 4_102_444_800_i64}),
            )
            .await;

        let (_, punishment) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({
                    "kind": "punishment",
                    "title": "extra day locked",
                    "effect_kind": "time_extension",
                    "time_extension_seconds": 86400
                }),
            )
            .await;
        let punishment_id = punishment["id"].as_str().unwrap().to_string();

        let (status, task) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/assignments"),
                serde_json::json!({
                    "kind": "task",
                    "title": "cold shower",
                    "completion_type": "proof_required",
                    "proof_media_types": ["video"],
                    "default_deadline_seconds": 3600,
                    "on_failure_template_id": punishment_id
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let task_id = task["id"].as_str().unwrap().to_string();

        let (status, submission) = submissive
            .post_multipart(
                &format!("/api/v1/submissive/assignments/{task_id}/proof"),
                &[("kind", "video")],
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let _ = submission;

        let (_, feed) = keyholder.get("/api/v1/keyholder/proof-submissions").await;
        let submission_id = feed[0]["id"].as_str().unwrap().to_string();

        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/proof-submissions/{submission_id}/review"),
                serde_json::json!({"status": "failed", "review_notes": "not convincing"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, updated_task) = keyholder
            .get(&format!("/api/v1/keyholder/assignments/{task_id}"))
            .await;
        assert_eq!(updated_task["status"], "failed");

        // Original target (2100-01-01T00:00:00Z) extended by the
        // punishment's 86400 seconds -> 2100-01-02T00:00:00Z.
        let (_, status_resp) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/status"))
            .await;
        assert_eq!(
            status_resp["target_release_at"],
            "2100-01-02T00:00:00+00:00"
        );
        let (_, all_assignments) = keyholder
            .get(&format!(
                "/api/v1/keyholder/submissives/{sub_id}/assignments"
            ))
            .await;
        let escalated = all_assignments
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["kind"] == "punishment")
            .unwrap();
        assert_eq!(escalated["status"], "applied");
        assert_eq!(escalated["escalated_from_assignment_id"], task_id);
    }

    #[tokio::test]
    async fn bare_grant_reward_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-grant@example.test",
            "sub-grant@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, reward) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/assignments"),
                serde_json::json!({"kind": "reward", "title": "movie night", "effect_kind": "grant"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reward["status"], "assigned");
        let reward_id = reward["id"].as_str().unwrap().to_string();

        let (status, _) = submissive
            .patch(
                &format!("/api/v1/submissive/assignments/{reward_id}/acknowledge"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/assignments/{reward_id}"),
                serde_json::json!({"status": "completed"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, own) = submissive.get("/api/v1/submissive/assignments").await;
        assert_eq!(own[0]["status"], "completed");
    }

    #[tokio::test]
    async fn deadline_sweep_auto_fails_an_overdue_acknowledge_only_task() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-sweep@example.test",
            "sub-sweep@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (_, task) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/assignments"),
                serde_json::json!({"kind": "task", "title": "message me by noon", "completion_type": "acknowledge_only", "default_deadline_seconds": 1}),
            )
            .await;
        let task_id = task["id"].as_str().unwrap().to_string();

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // The real sweeper wrapper (not just the domain tick), so its
        // notification dispatch (09-notifications.md §3) runs too.
        let pool_for_sweep = pool.clone();
        tokio::task::spawn_blocking(move || run_deadline_sweep_tick(&pool_for_sweep))
            .await
            .unwrap();

        let (_, updated) = keyholder
            .get(&format!("/api/v1/keyholder/assignments/{task_id}"))
            .await;
        assert_eq!(updated["status"], "failed");

        let (_, kh_feed) = keyholder.get("/api/v1/notifications").await;
        assert!(
            kh_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "task.failed")
        );
        let (_, sub_feed) = submissive.get("/api/v1/notifications").await;
        assert!(
            sub_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "task.failed")
        );
    }

    #[tokio::test]
    async fn toy_acquired_at_round_trips_through_patch_and_clears() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, _submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-acq@example.test", "sub-acq@example.test")
                .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, toy) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/toys"),
                serde_json::json!({"name": "steel cage", "acquired_at": "2025-01-01T00:00:00Z"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let toy_id = toy["id"].as_str().unwrap().to_string();
        assert!(
            toy["acquired_at"]
                .as_str()
                .unwrap()
                .starts_with("2025-01-01")
        );

        // PATCH can change it...
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/toys/{toy_id}"),
                serde_json::json!({"acquired_at": "2024-06-15T00:00:00Z"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/toys"))
            .await;
        assert!(
            list[0]["acquired_at"]
                .as_str()
                .unwrap()
                .starts_with("2024-06-15")
        );

        // ...and explicitly clear it.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/toys/{toy_id}"),
                serde_json::json!({"acquired_at": null}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/toys"))
            .await;
        assert!(list[0]["acquired_at"].is_null());
    }

    #[tokio::test]
    async fn toy_catalog_add_edit_request_removal_retire_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-toys@example.test", "sub-toys@example.test")
                .await;
        let sub_id = submissive_id(&mut keyholder).await;

        // Keyholder adds one.
        let (status, kh_toy) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/toys"),
                serde_json::json!({"name": "steel cage", "category": "chastity", "material": "steel"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let kh_toy_id = kh_toy["id"].as_str().unwrap().to_string();

        // Submissive adds their own.
        let (status, sub_toy) = submissive
            .post(
                "/api/v1/submissive/toys",
                serde_json::json!({"name": "bullet vibe", "tags": ["quiet", "travel-friendly"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let sub_toy_id = sub_toy["id"].as_str().unwrap().to_string();

        // Both roles see the same two-item catalog.
        let (_, list) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/toys"))
            .await;
        assert_eq!(list.as_array().unwrap().len(), 2);
        let (_, list) = submissive.get("/api/v1/submissive/toys").await;
        assert_eq!(list.as_array().unwrap().len(), 2);

        // Submissive can edit a toy the Keyholder added (12-toy-catalog.md §4).
        let (status, _) = submissive
            .patch(
                &format!("/api/v1/toys/{kh_toy_id}"),
                serde_json::json!({"storage_location": "bedside drawer"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Submissive can't retire outright, only request.
        let (status, _) = submissive
            .post(
                &format!("/api/v1/submissive/toys/{sub_toy_id}/request-removal"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        // Double-request is a conflict.
        let (status, _) = submissive
            .post(
                &format!("/api/v1/submissive/toys/{sub_toy_id}/request-removal"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        let (_, kh_feed) = keyholder.get("/api/v1/notifications").await;
        assert!(
            kh_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "toy.retirement_requested")
        );

        // Keyholder declines it — toy stays active, submissive notified.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/toys/{sub_toy_id}/decline-removal"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = submissive.get("/api/v1/submissive/toys").await;
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == sub_toy_id && t["retirement_requested_at"].is_null())
        );

        // Keyholder retires the other one directly.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/toys/{kh_toy_id}/retire"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, list) = submissive.get("/api/v1/submissive/toys").await;
        assert_eq!(list.as_array().unwrap().len(), 1);
        let (_, list) = submissive
            .get("/api/v1/submissive/toys?include_retired=true")
            .await;
        assert_eq!(list.as_array().unwrap().len(), 2);

        let (_, sub_feed) = submissive.get("/api/v1/notifications").await;
        let resolved_count = sub_feed
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["type"] == "toy.retirement_resolved")
            .count();
        assert_eq!(resolved_count, 2); // decline + retire

        // A second, unrelated keyholder can't reach either toy.
        seed_keyholder(
            &pool,
            "kh-toys-other@example.test",
            "correct horse battery staple",
        );
        let mut other_keyholder = TestClient::new(pool.clone());
        other_keyholder.get("/health").await;
        other_keyholder
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-toys-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other_keyholder
            .post(
                &format!("/api/v1/keyholder/toys/{sub_toy_id}/retire"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// `docs/16-mockup-implementation-gaps.md` item 2: the DB column and
    /// API field always existed, but nothing could ever populate them —
    /// no upload route, no UI. Covers upload, download, replace, delete,
    /// and cross-link scoping for the new `/toys/{id}/photo` endpoints.
    #[tokio::test]
    async fn toy_photo_upload_download_and_delete_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-toyphoto@example.test",
            "sub-toyphoto@example.test",
        )
        .await;

        let (_, toy) = submissive
            .post(
                "/api/v1/submissive/toys",
                serde_json::json!({"name": "bullet vibe"}),
            )
            .await;
        let toy_id = toy["id"].as_str().unwrap().to_string();
        assert!(toy["photo_url"].is_null());

        // A non-image content type is rejected.
        let (status, _) = submissive
            .post_multipart(
                &format!("/api/v1/toys/{toy_id}/photo"),
                &[],
                &[("photo", "notes.txt", "text/plain", b"not an image")],
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Upload a real photo.
        let (status, uploaded) = submissive
            .post_multipart(
                &format!("/api/v1/toys/{toy_id}/photo"),
                &[],
                &[("photo", "toy.png", "image/png", TINY_PNG)],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let photo_url = uploaded["photo_url"].as_str().unwrap().to_string();
        assert_eq!(photo_url, format!("/api/v1/toys/{toy_id}/photo"));

        // Both the submissive and their Keyholder can fetch it back.
        let (status, _) = submissive.get(&photo_url).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = keyholder.get(&photo_url).await;
        assert_eq!(status, StatusCode::OK);

        // Re-uploading replaces it (still exactly one photo per toy).
        let (status, replaced) = submissive
            .post_multipart(
                &format!("/api/v1/toys/{toy_id}/photo"),
                &[],
                &[("photo", "toy2.png", "image/png", TINY_PNG)],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replaced["photo_url"], photo_url);

        // Deleting clears it.
        let (status, _) = submissive.delete(&photo_url).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = submissive.get(&photo_url).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // An unrelated Keyholder can't reach this submissive's toy photo.
        seed_keyholder(
            &pool,
            "kh-toyphoto-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-toyphoto-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other_kh
            .post_multipart(
                &format!("/api/v1/toys/{toy_id}/photo"),
                &[],
                &[("photo", "toy.png", "image/png", TINY_PNG)],
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// Same optional-photo shape as toys (see the lifecycle test above),
    /// but set independently of check-in creation/`PATCH` rather than
    /// inline — a check-in's color/fields stay plain JSON either way.
    #[tokio::test]
    async fn checkin_photo_upload_download_and_delete_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-checkinphoto@example.test",
            "sub-checkinphoto@example.test",
        )
        .await;

        let (_, template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({
                    "title": "Morning cage check-in",
                    "auto_escalate_on_red": false,
                    "fields": []
                }),
            )
            .await;
        let template_id = template["id"].as_str().unwrap().to_string();

        let (_, checkin) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                ],
                &[],
            )
            .await;
        let checkin_id = checkin["id"].as_str().unwrap().to_string();
        assert!(checkin["photo_url"].is_null());

        // A non-image content type is rejected.
        let (status, _) = submissive
            .post_multipart(
                &format!("/api/v1/checkins/{checkin_id}/photo"),
                &[],
                &[("photo", "notes.txt", "text/plain", b"not an image")],
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Upload a real photo.
        let (status, uploaded) = submissive
            .post_multipart(
                &format!("/api/v1/checkins/{checkin_id}/photo"),
                &[],
                &[("photo", "checkin.png", "image/png", TINY_PNG)],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let photo_url = uploaded["photo_url"].as_str().unwrap().to_string();
        assert_eq!(photo_url, format!("/api/v1/checkins/{checkin_id}/photo"));

        // Both the submissive and their Keyholder can fetch it back.
        let (status, _) = submissive.get(&photo_url).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = keyholder.get(&photo_url).await;
        assert_eq!(status, StatusCode::OK);

        // Re-uploading replaces it (still exactly one photo per check-in).
        let (status, replaced) = submissive
            .post_multipart(
                &format!("/api/v1/checkins/{checkin_id}/photo"),
                &[],
                &[("photo", "checkin2.png", "image/png", TINY_PNG)],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replaced["photo_url"], photo_url);

        // Deleting clears it.
        let (status, _) = submissive.delete(&photo_url).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = submissive.get(&photo_url).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // An unrelated Keyholder can't reach this submissive's check-in photo.
        seed_keyholder(
            &pool,
            "kh-checkinphoto-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-checkinphoto-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other_kh
            .post_multipart(
                &format!("/api/v1/checkins/{checkin_id}/photo"),
                &[],
                &[("photo", "checkin.png", "image/png", TINY_PNG)],
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// `photo` as a template field type — a required one has to be
    /// enforced at creation time (unlike a purely-optional attachment),
    /// which only works because create now accepts multipart with an
    /// inline `photo` part rather than plain JSON.
    #[tokio::test]
    async fn checkin_required_photo_field_is_enforced_at_creation() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-requiredphoto@example.test",
            "sub-requiredphoto@example.test",
        )
        .await;

        let (_, template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({
                    "title": "Proof-required check-in",
                    "auto_escalate_on_red": false,
                    "fields": [
                        {"field_key": "proof", "label": "Proof photo", "field_type": "photo", "config": {}, "required": true}
                    ]
                }),
            )
            .await;
        let template_id = template["id"].as_str().unwrap().to_string();

        // No photo attached — the required field is missing.
        let (status, _) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                ],
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // With a photo attached in the same request, it's accepted and
        // the photo is already live on the response, no follow-up upload needed.
        let (status, checkin) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                ],
                &[("photo", "proof.png", "image/png", TINY_PNG)],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let checkin_id = checkin["id"].as_str().unwrap().to_string();
        let photo_url = checkin["photo_url"].as_str().unwrap().to_string();
        assert_eq!(photo_url, format!("/api/v1/checkins/{checkin_id}/photo"));
        let (status, _) = submissive.get(&photo_url).await;
        assert_eq!(status, StatusCode::OK);
    }

    /// The `photo` field type now also accepts a video — same slot,
    /// content-type just disambiguates what got stored (13-checkins.md,
    /// "photo/video" combined field type).
    #[tokio::test]
    async fn checkin_photo_field_accepts_a_video_file() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-checkinvideo@example.test",
            "sub-checkinvideo@example.test",
        )
        .await;

        let (_, template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({
                    "title": "Video check-in",
                    "auto_escalate_on_red": false,
                    "fields": []
                }),
            )
            .await;
        let template_id = template["id"].as_str().unwrap().to_string();

        let (status, checkin) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                ],
                &[(
                    "photo",
                    "clip.mp4",
                    "video/mp4",
                    b"pretend this is an mp4 container",
                )],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let photo_url = checkin["photo_url"].as_str().unwrap().to_string();
        let (status, _) = submissive.get(&photo_url).await;
        assert_eq!(status, StatusCode::OK);
    }

    /// The `audio` field also accepts mp3 and wav, not just webm/mp4.
    #[tokio::test]
    async fn checkin_audio_field_accepts_mp3_and_wav() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-checkinmp3@example.test",
            "sub-checkinmp3@example.test",
        )
        .await;

        let (_, template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({
                    "title": "MP3/WAV check-in",
                    "auto_escalate_on_red": false,
                    "fields": []
                }),
            )
            .await;
        let template_id = template["id"].as_str().unwrap().to_string();

        let (status, checkin) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                ],
                &[(
                    "audio",
                    "voice.mp3",
                    "audio/mpeg",
                    b"pretend this is an mp3 frame",
                )],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let audio_url = checkin["audio_url"].as_str().unwrap().to_string();
        let (status, _) = submissive.get(&audio_url).await;
        assert_eq!(status, StatusCode::OK);

        let (status, replaced) = submissive
            .post_multipart(
                &format!("/api/v1/checkins/{}/audio", checkin["id"].as_str().unwrap()),
                &[],
                &[(
                    "audio",
                    "voice.wav",
                    "audio/wav",
                    b"pretend this is a wav riff",
                )],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replaced["audio_url"], audio_url);
    }

    /// Same optional-attachment shape as the photo lifecycle test, for the
    /// independent `audio` slot (voice memos).
    #[tokio::test]
    async fn checkin_audio_upload_download_and_delete_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-checkinaudio@example.test",
            "sub-checkinaudio@example.test",
        )
        .await;

        let (_, template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({
                    "title": "Voice check-in",
                    "auto_escalate_on_red": false,
                    "fields": []
                }),
            )
            .await;
        let template_id = template["id"].as_str().unwrap().to_string();

        let (_, checkin) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                ],
                &[],
            )
            .await;
        let checkin_id = checkin["id"].as_str().unwrap().to_string();
        assert!(checkin["audio_url"].is_null());

        // A non-audio content type is rejected.
        let (status, _) = submissive
            .post_multipart(
                &format!("/api/v1/checkins/{checkin_id}/audio"),
                &[],
                &[("audio", "notes.txt", "text/plain", b"not audio")],
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Upload a voice memo.
        let (status, uploaded) = submissive
            .post_multipart(
                &format!("/api/v1/checkins/{checkin_id}/audio"),
                &[],
                &[(
                    "audio",
                    "memo.weba",
                    "audio/webm",
                    b"pretend this is webm audio",
                )],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let audio_url = uploaded["audio_url"].as_str().unwrap().to_string();
        assert_eq!(audio_url, format!("/api/v1/checkins/{checkin_id}/audio"));

        // Both the submissive and their Keyholder can fetch it back.
        let (status, _) = submissive.get(&audio_url).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = keyholder.get(&audio_url).await;
        assert_eq!(status, StatusCode::OK);

        // Re-uploading replaces it (still exactly one voice memo per check-in).
        let (status, replaced) = submissive
            .post_multipart(
                &format!("/api/v1/checkins/{checkin_id}/audio"),
                &[],
                &[(
                    "audio",
                    "memo2.m4a",
                    "audio/mp4",
                    b"pretend this is m4a audio",
                )],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replaced["audio_url"], audio_url);

        // Deleting clears it.
        let (status, _) = submissive.delete(&audio_url).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = submissive.get(&audio_url).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // An unrelated Keyholder can't reach this submissive's check-in audio.
        seed_keyholder(
            &pool,
            "kh-checkinaudio-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-checkinaudio-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other_kh
            .post_multipart(
                &format!("/api/v1/checkins/{checkin_id}/audio"),
                &[],
                &[(
                    "audio",
                    "memo.weba",
                    "audio/webm",
                    b"pretend this is webm audio",
                )],
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// `audio` as a template field type — mirrors the required-photo
    /// enforcement test, for the independent voice-memo slot.
    #[tokio::test]
    async fn checkin_required_audio_field_is_enforced_at_creation() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-requiredaudio@example.test",
            "sub-requiredaudio@example.test",
        )
        .await;

        let (_, template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({
                    "title": "Voice-required check-in",
                    "auto_escalate_on_red": false,
                    "fields": [
                        {"field_key": "voice", "label": "Voice memo", "field_type": "audio", "config": {}, "required": true}
                    ]
                }),
            )
            .await;
        let template_id = template["id"].as_str().unwrap().to_string();

        // No audio attached — the required field is missing.
        let (status, _) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                ],
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // With audio attached in the same request, it's accepted and
        // already live on the response, no follow-up upload needed.
        let (status, checkin) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                ],
                &[(
                    "audio",
                    "voice.weba",
                    "audio/webm",
                    b"pretend this is webm audio",
                )],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let checkin_id = checkin["id"].as_str().unwrap().to_string();
        let audio_url = checkin["audio_url"].as_str().unwrap().to_string();
        assert_eq!(audio_url, format!("/api/v1/checkins/{checkin_id}/audio"));
        let (status, _) = submissive.get(&audio_url).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn points_earn_manual_adjust_and_redeem_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-points@example.test",
            "sub-points@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        // Points are off by default — the catalog earns nothing yet.
        let (_, points) = submissive.get("/api/v1/submissive/points").await;
        assert_eq!(points["enabled"], false);
        assert_eq!(points["balance"], 0);

        // Turn points on (extends the existing link-settings endpoint).
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/link/settings"),
                serde_json::json!({"self_report_allowed": false, "catalog_visible_to_submissive": true, "points_enabled": true}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // A task worth 5 points, completed, earns them.
        let (_, task) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/assignments"),
                serde_json::json!({"kind": "task", "title": "tidy up", "completion_type": "acknowledge_only", "default_deadline_seconds": 3600, "points_delta": 5}),
            )
            .await;
        let task_id = task["id"].as_str().unwrap().to_string();
        submissive
            .patch(
                &format!("/api/v1/submissive/assignments/{task_id}/acknowledge"),
                serde_json::json!({}),
            )
            .await;
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/assignments/{task_id}"),
                serde_json::json!({"status": "completed"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, points) = submissive.get("/api/v1/submissive/points").await;
        assert_eq!(points["balance"], 5);
        assert!(
            points["transactions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["reason"] == "task_completed" && t["delta"] == 5)
        );

        // Manual adjustment.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/points/adjust"),
                serde_json::json!({"delta": 20, "notes": "bonus for good behavior"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, points) = submissive.get("/api/v1/submissive/points").await;
        assert_eq!(points["balance"], 25);

        // A reward template costing 20 points.
        let (_, reward) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({"kind": "reward", "title": "movie night", "effect_kind": "grant", "points_cost": 20}),
            )
            .await;
        let reward_id = reward["id"].as_str().unwrap().to_string();

        // The submissive can browse the catalog (a documented but
        // previously-unbuilt endpoint) to find what's redeemable.
        let (status, catalog) = submissive.get("/api/v1/submissive/templates").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            catalog
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == reward_id && t["points_cost"] == 20)
        );

        let (status, request) = submissive
            .post(
                &format!("/api/v1/submissive/rewards/{reward_id}/redeem"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(request["status"], "pending");
        assert_eq!(request["reward_title"], "movie night");
        assert_eq!(request["submissive_display_name"], "Sub");
        let request_id = request["id"].as_str().unwrap().to_string();

        let (_, kh_feed) = keyholder.get("/api/v1/notifications").await;
        assert!(
            kh_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "points.redemption_requested")
        );

        let (_, pending) = keyholder
            .get("/api/v1/keyholder/reward-redemption-requests")
            .await;
        assert_eq!(pending.as_array().unwrap().len(), 1);
        assert_eq!(pending[0]["reward_title"], "movie night");
        assert_eq!(pending[0]["submissive_display_name"], "Sub");

        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/reward-redemption-requests/{request_id}"),
                serde_json::json!({"decision": "approve"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, points) = submissive.get("/api/v1/submissive/points").await;
        assert_eq!(points["balance"], 5); // 25 - 20

        let (_, assignments) = submissive.get("/api/v1/submissive/assignments").await;
        assert!(
            assignments
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["title"] == "movie night")
        );

        let (_, sub_feed) = submissive.get("/api/v1/notifications").await;
        assert!(
            sub_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "points.redemption_resolved")
        );

        // Can't decide the same request twice.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/reward-redemption-requests/{request_id}"),
                serde_json::json!({"decision": "deny"}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // Redeeming with insufficient balance is rejected.
        let (status, _) = submissive
            .post(
                &format!("/api/v1/submissive/rewards/{reward_id}/redeem"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn checkin_template_create_and_fill_in_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-checkin@example.test",
            "sub-checkin@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({
                    "title": "Morning cage check-in",
                    "auto_escalate_on_red": false,
                    "fields": [
                        {"field_key": "skin_status", "label": "Skin status", "field_type": "select", "config": {"options": ["normal", "chafing"]}, "required": true},
                        {"field_key": "notes", "label": "Notes", "field_type": "text", "config": {}, "required": false}
                    ]
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let template_id = template["id"].as_str().unwrap().to_string();
        assert_eq!(template["fields"].as_array().unwrap().len(), 2);

        // Submissive can see it (catalog visible by default).
        let (status, sub_templates) = submissive.get("/api/v1/submissive/checkin-templates").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            sub_templates
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == template_id)
        );

        // Missing the required field is rejected.
        let (status, _) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                ],
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // A normal green check-in submits fine and notifies the keyholder, feed-only.
        let (status, checkin) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "green"),
                    ("field_values", r#"{"skin_status": "normal"}"#),
                ],
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let checkin_id = checkin["id"].as_str().unwrap().to_string();

        let (_, kh_feed) = keyholder.get("/api/v1/notifications").await;
        assert!(
            kh_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "checkin.submitted")
        );

        // Keyholder can list it, filtered by color.
        let (status, list) = keyholder
            .get(&format!(
                "/api/v1/keyholder/submissives/{sub_id}/checkins?color=green"
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);

        // Editing it to red WITHOUT auto-escalation sends a strong
        // checkin.red_flag, not a safety alert.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/checkins/{checkin_id}"),
                serde_json::json!({"color": "red"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, kh_feed) = keyholder.get("/api/v1/notifications").await;
        assert!(
            kh_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "checkin.red_flag")
        );
        let (_, alerts) = keyholder.get("/api/v1/keyholder/safety-alerts").await;
        assert_eq!(alerts.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn checkin_red_with_auto_escalate_raises_a_safety_alert_not_just_a_notification() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-checkin2@example.test",
            "sub-checkin2@example.test",
        )
        .await;

        let (_, template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({
                    "title": "Estim mid-session check-in",
                    "auto_escalate_on_red": true,
                    "fields": [{"field_key": "arousal", "label": "Arousal", "field_type": "scale", "config": {"min": 0, "max": 10}, "required": false}]
                }),
            )
            .await;
        let template_id = template["id"].as_str().unwrap().to_string();

        let (status, _) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", template_id.as_str()),
                    ("color", "red"),
                    ("field_values", r#"{"arousal": 8}"#),
                ],
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let (_, alerts) = keyholder.get("/api/v1/keyholder/safety-alerts").await;
        let alerts = alerts.as_array().unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["raised_via"], "system");

        // The keyholder gets a safety.alert_raised notification, not a
        // separate checkin.red_flag for the same event.
        let (_, kh_feed) = keyholder.get("/api/v1/notifications").await;
        let kh_feed = kh_feed.as_array().unwrap();
        assert!(kh_feed.iter().any(|n| n["type"] == "safety.alert_raised"));
        assert!(!kh_feed.iter().any(|n| n["type"] == "checkin.red_flag"));
    }

    #[tokio::test]
    async fn play_session_template_visibility_and_toy_ownership_validation() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-play1@example.test",
            "sub-play1@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (status, template) = keyholder
            .post(
                "/api/v1/keyholder/play-session-templates",
                serde_json::json!({
                    "title": "Standard scene",
                    "suggested_toy_categories": ["vibrator", "cock cage"],
                    "planned_duration_seconds": 3600
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let template_id = template["id"].as_str().unwrap().to_string();

        let (status, sub_templates) = submissive
            .get("/api/v1/submissive/play-session-templates")
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            sub_templates
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == template_id)
        );

        // A toy that doesn't belong to this submissive is rejected.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/play-sessions"),
                serde_json::json!({
                    "template_id": template_id,
                    "toy_ids": ["not-a-real-toy-id"]
                }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        // A toy that does belong to the submissive works fine.
        let (_, toy) = submissive
            .post(
                "/api/v1/submissive/toys",
                serde_json::json!({"name": "steel cage"}),
            )
            .await;
        let toy_id = toy["id"].as_str().unwrap().to_string();

        let (status, session) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/play-sessions"),
                serde_json::json!({
                    "template_id": template_id,
                    "toy_ids": [toy_id]
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(session["status"], "scheduled");
        assert_eq!(session["title"], "Standard scene");
        assert_eq!(session["toy_ids"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn play_session_live_start_end_judge_complete_lifecycle_with_notifications() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-play2@example.test",
            "sub-play2@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (_, session) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/play-sessions"),
                serde_json::json!({"title": "Live scene"}),
            )
            .await;
        let session_id = session["id"].as_str().unwrap().to_string();
        assert_eq!(session["status"], "scheduled");

        // Submissive starts it — the keyholder gets notified.
        let (status, started) = submissive
            .post(
                &format!("/api/v1/submissive/play-sessions/{session_id}/start"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(started["status"], "in_progress");
        let (_, kh_feed) = keyholder.get("/api/v1/notifications").await;
        assert!(
            kh_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "play_session.started")
        );

        // Can't start twice.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/play-sessions/{session_id}/start"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // Keyholder ends it — the keyholder's own queue notification fires.
        let (status, ended) = keyholder
            .post(
                &format!("/api/v1/keyholder/play-sessions/{session_id}/end"),
                serde_json::json!({"safety_check_ok": true}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ended["status"], "pending_judgement");
        assert_eq!(ended["safety_check_ok"], true);
        let (_, kh_feed) = keyholder.get("/api/v1/notifications").await;
        assert!(
            kh_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "play_session.pending_judgement")
        );

        // Judgement creates a reward assignment tied back to this session.
        let (status, judged) = keyholder
            .patch(
                &format!("/api/v1/keyholder/play-sessions/{session_id}/judgement"),
                serde_json::json!({
                    "judgement_notes": "Went great",
                    "reward": {
                        "title": "Extra praise",
                        "description": "Well done",
                        "effect_kind": "time_reduction",
                        "time_reduction_seconds": 3600
                    }
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(judged["judgement_notes"], "Went great");
        let reward_assignment_id = judged["reward_assignment_id"].as_str().unwrap().to_string();

        let (_, assignment) = keyholder
            .get(&format!(
                "/api/v1/keyholder/submissives/{sub_id}/assignments"
            ))
            .await;
        let assignment = assignment
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == reward_assignment_id)
            .expect("reward assignment exists");
        assert_eq!(assignment["triggered_by_play_session_id"], session_id);

        // No judged notification yet — that fires on complete, not judgement.
        let (_, sub_feed) = submissive.get("/api/v1/notifications").await;
        assert!(
            !sub_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "play_session.judged")
        );

        let (status, completed) = keyholder
            .patch(
                &format!("/api/v1/keyholder/play-sessions/{session_id}/complete"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(completed["status"], "completed");

        let (_, sub_feed) = submissive.get("/api/v1/notifications").await;
        assert!(
            sub_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "play_session.judged")
        );

        // Can't complete twice, and judgement is rejected once completed.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/play-sessions/{session_id}/complete"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/play-sessions/{session_id}/judgement"),
                serde_json::json!({"judgement_notes": "too late"}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn play_session_retrospective_entry_and_cancel_and_checkin_schedule() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-play3@example.test",
            "sub-play3@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        // Supplying both started_at and ended_at lands directly in
        // pending_judgement (14-play-sessions.md §3).
        let (status, retro) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/play-sessions"),
                serde_json::json!({
                    "title": "Logged after the fact",
                    "started_at": "2024-01-01T00:00:00Z",
                    "ended_at": "2024-01-01T01:00:00Z"
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(retro["status"], "pending_judgement");

        // A scheduled session can be cancelled.
        let (_, scheduled) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/play-sessions"),
                serde_json::json!({"title": "To be cancelled"}),
            )
            .await;
        let scheduled_id = scheduled["id"].as_str().unwrap().to_string();
        let (status, cancelled) = keyholder
            .patch(
                &format!("/api/v1/keyholder/play-sessions/{scheduled_id}/cancel"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cancelled["status"], "cancelled");
        // Cancelling again is rejected.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/play-sessions/{scheduled_id}/cancel"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // A session with a check-in template + interval + duration
        // generates a schedule, and a matching check-in fulfills the
        // earliest open slot (14-play-sessions.md §4).
        let (_, checkin_template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({
                    "title": "Mid-scene check",
                    "auto_escalate_on_red": false,
                    "fields": []
                }),
            )
            .await;
        let checkin_template_id = checkin_template["id"].as_str().unwrap().to_string();

        let (_, scheduled_session) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/play-sessions"),
                serde_json::json!({
                    "title": "With mid-session checks",
                    "planned_duration_seconds": 2400,
                    "checkin_template_id": checkin_template_id,
                    "checkin_interval_seconds": 1200
                }),
            )
            .await;
        let session_id = scheduled_session["id"].as_str().unwrap().to_string();
        let schedule = scheduled_session["checkin_schedule"].as_array().unwrap();
        assert_eq!(schedule.len(), 2);
        assert!(schedule.iter().all(|s| s["fulfilled_checkin_id"].is_null()));

        let (status, _) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", checkin_template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                    ("related_play_session_id", session_id.as_str()),
                ],
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let (_, detail) = keyholder
            .get(&format!("/api/v1/keyholder/play-sessions/{session_id}"))
            .await;
        let schedule = detail["checkin_schedule"].as_array().unwrap();
        assert!(!schedule[0]["fulfilled_checkin_id"].is_null());
        assert!(schedule[1]["fulfilled_checkin_id"].is_null());
    }

    /// 13-checkins.md §5: the live-session check-in SSE stream. Covers
    /// the gating (refused unless `in_progress`, and unless the caller
    /// is on the session's own link) plus the two real events it ever
    /// sends: a `checkin_update` when a matching check-in is written,
    /// and a closing `session_ended` once the session leaves
    /// `in_progress`, after which the connection itself closes.
    #[tokio::test]
    async fn play_session_checkin_sse_stream_delivers_updates_and_closes_on_end() {
        use http_body_util::BodyExt;

        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) =
            linked_keyholder_and_submissive(&pool, "kh-sse@example.test", "sub-sse@example.test")
                .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (_, checkin_template) = keyholder
            .post(
                "/api/v1/keyholder/checkin-templates",
                serde_json::json!({"title": "Live check", "auto_escalate_on_red": false, "fields": []}),
            )
            .await;
        let checkin_template_id = checkin_template["id"].as_str().unwrap().to_string();

        // A scheduled (not yet started) session's stream is refused.
        let (_, scheduled_session) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/play-sessions"),
                serde_json::json!({"title": "Not started"}),
            )
            .await;
        let scheduled_id = scheduled_session["id"].as_str().unwrap().to_string();
        let scheduled_resp = keyholder
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/v1/play-sessions/{scheduled_id}/checkin-stream"
                    ))
                    .header(header::COOKIE, keyholder.cookie_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(scheduled_resp.status(), StatusCode::CONFLICT);

        // Start a live session.
        let (_, session) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/play-sessions"),
                serde_json::json!({"title": "Live scene", "started_at": "2024-01-01T00:00:00Z"}),
            )
            .await;
        let session_id = session["id"].as_str().unwrap().to_string();
        assert_eq!(session["status"], "in_progress");

        // An unrelated keyholder can't open the stream — 404, not 403.
        seed_keyholder(
            &pool,
            "kh-sse-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-sse-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let other_resp = other_kh
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/play-sessions/{session_id}/checkin-stream"))
                    .header(header::COOKIE, other_kh.cookie_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(other_resp.status(), StatusCode::NOT_FOUND);

        // Open the real stream as the keyholder.
        let stream_resp = keyholder
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/play-sessions/{session_id}/checkin-stream"))
                    .header(header::COOKIE, keyholder.cookie_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stream_resp.status(), StatusCode::OK);
        let content_type = stream_resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(content_type.starts_with("text/event-stream"));
        let body = stream_resp.into_body();

        // Reading frames runs as a background task so the submissive's
        // checkin POST below can happen concurrently, the same way a
        // real second browser tab would.
        let read_task = tokio::spawn(async move {
            let mut body = body;
            let mut collected = String::new();
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(5), body.frame()).await {
                    Ok(Some(Ok(frame))) => {
                        if let Some(bytes) = frame.data_ref() {
                            collected.push_str(&String::from_utf8_lossy(bytes));
                            if collected.contains("event: checkin_update") {
                                return (collected, Some(body));
                            }
                        }
                    }
                    _ => return (collected, None),
                }
            }
        });

        // Give the stream a moment to actually register its
        // subscription before publishing — the same race any real
        // client has.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (status, _) = submissive
            .post_multipart(
                "/api/v1/submissive/checkins",
                &[
                    ("template_id", checkin_template_id.as_str()),
                    ("color", "green"),
                    ("field_values", "{}"),
                    ("related_play_session_id", session_id.as_str()),
                ],
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let (collected, body) = tokio::time::timeout(std::time::Duration::from_secs(5), read_task)
            .await
            .expect("read task did not finish in time")
            .unwrap();
        assert!(
            collected.contains("event: checkin_update"),
            "expected a checkin_update SSE event, got: {collected}"
        );
        let mut body = body.expect("stream should still be open after one event");

        // Ending the session sends the closing event and the stream
        // itself ends — nothing is "live" once status leaves
        // in_progress (13-checkins.md §5).
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/play-sessions/{session_id}/end"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);

        let mut collected = String::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Some(bytes) = frame.data_ref() {
                        collected.push_str(&String::from_utf8_lossy(bytes));
                    }
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => panic!("stream error: {e}"),
                Err(_) => panic!("stream did not close after the session ended"),
            }
        }
        assert!(
            collected.contains("event: session_ended"),
            "expected a session_ended SSE event, got: {collected}"
        );
    }

    /// 06-future-extensions.md §9: the structured hard/soft limits
    /// catalog and per-submissive ratings — global seed visibility, a
    /// Keyholder's own additions scoped to only their own submissives,
    /// the submissive-owned rating lifecycle (set/upsert/clear), and
    /// the same read-only visibility split the free-text limits fields
    /// already have.
    #[tokio::test]
    async fn structured_limits_catalog_and_rating_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-limits@example.test",
            "sub-limits@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        // Global seed items are visible to a fresh Keyholder with no
        // additions of their own yet.
        let (status, items) = keyholder.get("/api/v1/keyholder/limit-items").await;
        assert_eq!(status, StatusCode::OK);
        let items = items.as_array().unwrap();
        assert!(items.iter().any(|i| i["id"] == "seed-impact-paddle"));
        assert!(items.iter().all(|i| i["is_global"] == true));

        // Keyholder adds their own item.
        let (status, custom) = keyholder
            .post(
                "/api/v1/keyholder/limit-items",
                serde_json::json!({"category": "House rules", "label": "No phone during scenes"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(custom["is_global"], false);
        let custom_id = custom["id"].as_str().unwrap().to_string();

        // A second, unrelated Keyholder never sees it, and can't edit it.
        seed_keyholder(
            &pool,
            "kh-limits-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-limits-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (_, other_items) = other_kh.get("/api/v1/keyholder/limit-items").await;
        assert!(
            !other_items
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["id"] == custom_id)
        );
        let (status, _) = other_kh
            .patch(
                &format!("/api/v1/keyholder/limit-items/{custom_id}"),
                serde_json::json!({"label": "hijacked"}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // Nor can anyone edit a global seed item through this path.
        let (status, _) = keyholder
            .patch(
                "/api/v1/keyholder/limit-items/seed-impact-paddle",
                serde_json::json!({"label": "hijacked"}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Submissive sees both the seed items and the Keyholder's own
        // addition, all unrated so far.
        let (status, sub_items) = submissive.get("/api/v1/submissive/limit-items").await;
        assert_eq!(status, StatusCode::OK);
        let sub_items = sub_items.as_array().unwrap();
        assert!(sub_items.iter().any(|i| i["id"] == custom_id));
        assert!(sub_items.iter().all(|i| i["rating"].is_null()));

        // Invalid rating value is rejected.
        let (status, _) = submissive
            .request(
                "PUT",
                "/api/v1/submissive/limit-ratings/seed-impact-paddle",
                Some(serde_json::json!({"rating": "bogus"})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Submissive rates the seed item hard and their own custom item soft.
        let (status, _) = submissive
            .request(
                "PUT",
                "/api/v1/submissive/limit-ratings/seed-impact-paddle",
                Some(serde_json::json!({"rating": "hard", "notes": "never"})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = submissive
            .request(
                "PUT",
                &format!("/api/v1/submissive/limit-ratings/{custom_id}"),
                Some(serde_json::json!({"rating": "soft"})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Upserting the same item replaces rather than duplicating.
        let (status, _) = submissive
            .request(
                "PUT",
                "/api/v1/submissive/limit-ratings/seed-impact-paddle",
                Some(serde_json::json!({"rating": "soft"})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, sub_items) = submissive.get("/api/v1/submissive/limit-items").await;
        let paddle = sub_items
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == "seed-impact-paddle")
            .unwrap();
        assert_eq!(paddle["rating"], "soft");

        // The Keyholder sees the same ratings, read-only, for this submissive.
        let (status, kh_view) = keyholder
            .get(&format!(
                "/api/v1/keyholder/submissives/{sub_id}/limit-ratings"
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        let kh_view = kh_view.as_array().unwrap();
        let paddle = kh_view
            .iter()
            .find(|i| i["id"] == "seed-impact-paddle")
            .unwrap();
        assert_eq!(paddle["rating"], "soft");
        let custom_view = kh_view.iter().find(|i| i["id"] == custom_id).unwrap();
        assert_eq!(custom_view["rating"], "soft");
        // Still-unrated items show up too, explicitly not-rated rather
        // than omitted.
        assert!(
            kh_view
                .iter()
                .any(|i| i["id"] == "seed-impact-cane" && i["rating"].is_null())
        );

        // Clearing a rating returns it to "not discussed."
        let (status, _) = submissive
            .request(
                "DELETE",
                "/api/v1/submissive/limit-ratings/seed-impact-paddle",
                None,
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, sub_items) = submissive.get("/api/v1/submissive/limit-items").await;
        let paddle = sub_items
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == "seed-impact-paddle")
            .unwrap();
        assert!(paddle["rating"].is_null());
    }

    /// Limits & boundaries redesign: a Keyholder rates their own catalog
    /// the same way a submissive rates theirs — same endpoints shape,
    /// same "no row = not discussed" semantics, but scoped to the
    /// Keyholder's own id on both sides (catalog owner and rating owner
    /// are the same person here).
    #[tokio::test]
    async fn keyholder_can_rate_their_own_limits_scoped_to_themselves() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-selflimits@example.test",
            "sub-selflimits@example.test",
        )
        .await;

        // Initially unrated, all seed items visible.
        let (status, items) = keyholder.get("/api/v1/keyholder/limit-ratings").await;
        assert_eq!(status, StatusCode::OK);
        let items = items.as_array().unwrap();
        assert!(items.iter().any(|i| i["id"] == "seed-impact-paddle"));
        assert!(items.iter().all(|i| i["rating"].is_null()));

        // A submissive can't touch these endpoints at all.
        let (status, _) = submissive.get("/api/v1/keyholder/limit-ratings").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = submissive
            .request(
                "PUT",
                "/api/v1/keyholder/limit-ratings/seed-impact-paddle",
                Some(serde_json::json!({"rating": "hard"})),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Invalid rating rejected.
        let (status, _) = keyholder
            .request(
                "PUT",
                "/api/v1/keyholder/limit-ratings/seed-impact-paddle",
                Some(serde_json::json!({"rating": "bogus"})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Rate one item, then upsert it to a different tier.
        let (status, _) = keyholder
            .request(
                "PUT",
                "/api/v1/keyholder/limit-ratings/seed-impact-paddle",
                Some(serde_json::json!({"rating": "hard", "notes": "never"})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = keyholder
            .request(
                "PUT",
                "/api/v1/keyholder/limit-ratings/seed-impact-paddle",
                Some(serde_json::json!({"rating": "soft"})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, items) = keyholder.get("/api/v1/keyholder/limit-ratings").await;
        let paddle = items
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == "seed-impact-paddle")
            .unwrap();
        assert_eq!(paddle["rating"], "soft");

        // A different Keyholder rating their own catalog never affects
        // this one's ratings.
        seed_keyholder(
            &pool,
            "kh-selflimits-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-selflimits-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        other_kh
            .request(
                "PUT",
                "/api/v1/keyholder/limit-ratings/seed-impact-paddle",
                Some(serde_json::json!({"rating": "okay"})),
            )
            .await;
        let (_, items) = keyholder.get("/api/v1/keyholder/limit-ratings").await;
        let paddle = items
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == "seed-impact-paddle")
            .unwrap();
        assert_eq!(paddle["rating"], "soft");

        // Clearing returns to "not discussed."
        let (status, _) = keyholder
            .request(
                "DELETE",
                "/api/v1/keyholder/limit-ratings/seed-impact-paddle",
                None,
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, items) = keyholder.get("/api/v1/keyholder/limit-ratings").await;
        let paddle = items
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == "seed-impact-paddle")
            .unwrap();
        assert!(paddle["rating"].is_null());
    }

    /// 06-future-extensions.md §14: repeating tasks. Covers rule
    /// creation validation (must be a task template, must belong to
    /// this Keyholder), listing, editing, and — via the real sweeper
    /// wrapper so notification dispatch runs too — an actual spawn
    /// that produces an ordinary task assignment and a task.assigned
    /// notification indistinguishable from a manual one.
    #[tokio::test]
    async fn recurring_task_rule_lifecycle_and_sweep_spawns_an_ordinary_task() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-recur@example.test",
            "sub-recur@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (_, reward_template) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({"kind": "reward", "title": "Nice job", "effect_kind": "grant"}),
            )
            .await;
        let reward_template_id = reward_template["id"].as_str().unwrap().to_string();

        // A reward template is rejected — a rule only ever spawns tasks.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/recurring-tasks"),
                serde_json::json!({
                    "template_id": reward_template_id,
                    "recurrence_kind": "interval_hours",
                    "recurrence_value": {"hours": 6}
                }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let (_, task_template) = keyholder
            .post(
                "/api/v1/keyholder/templates",
                serde_json::json!({
                    "kind": "task", "title": "Morning cage photo",
                    "completion_type": "acknowledge_only", "default_deadline_seconds": 3600
                }),
            )
            .await;
        let task_template_id = task_template["id"].as_str().unwrap().to_string();

        // Malformed recurrence is rejected up front.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/recurring-tasks"),
                serde_json::json!({
                    "template_id": task_template_id,
                    "recurrence_kind": "bogus_kind",
                    "recurrence_value": {}
                }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, rule) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/recurring-tasks"),
                serde_json::json!({
                    "template_id": task_template_id,
                    "recurrence_kind": "interval_hours",
                    "recurrence_value": {"hours": 6}
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(rule["active"], true);
        assert_eq!(rule["allow_overlap"], false);
        let rule_id = rule["id"].as_str().unwrap().to_string();

        let (status, list) = keyholder
            .get(&format!(
                "/api/v1/keyholder/submissives/{sub_id}/recurring-tasks"
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);

        // Deactivating is the only retirement path — no delete endpoint.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/recurring-tasks/{rule_id}"),
                serde_json::json!({"active": false}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list) = keyholder
            .get(&format!(
                "/api/v1/keyholder/submissives/{sub_id}/recurring-tasks"
            ))
            .await;
        assert_eq!(list[0]["active"], false);

        // Reactivate and back-date next_due_at directly so the sweep
        // picks it up immediately, then run the real sweeper wrapper
        // (not just the domain tick) so its notification dispatch runs.
        keyholder
            .patch(
                &format!("/api/v1/keyholder/recurring-tasks/{rule_id}"),
                serde_json::json!({"active": true}),
            )
            .await;
        {
            let conn = pool.get().unwrap();
            conn.execute(
                "UPDATE recurring_task_rules SET next_due_at = ?1 WHERE id = ?2",
                rusqlite::params![crate::auth::session::now() - 10, rule_id],
            )
            .unwrap();
        }

        let pool_for_sweep = pool.clone();
        tokio::task::spawn_blocking(move || run_deadline_sweep_tick(&pool_for_sweep))
            .await
            .unwrap();

        let (_, assignments) = keyholder
            .get(&format!(
                "/api/v1/keyholder/submissives/{sub_id}/assignments"
            ))
            .await;
        let spawned = assignments
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["title"] == "Morning cage photo")
            .expect("the sweep spawned the task");
        assert_eq!(spawned["assigned_via"], "system");
        assert_eq!(
            spawned["spawned_by_recurring_task_rule_id"],
            rule_id.as_str()
        );

        // The sweep fires the notification via a detached `tokio::spawn`
        // (it's a sync function running inside `spawn_blocking`, with no
        // async context to `.await` it from directly) — give it a moment
        // to actually run before checking the feed.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let (_, sub_feed) = submissive.get("/api/v1/notifications").await;
        assert!(
            sub_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "task.assigned"
                    && n["title"] == "New task: Morning cage photo")
        );

        // A second sweep right away spawns nothing further (skip-if-open).
        let pool_for_sweep = pool.clone();
        tokio::task::spawn_blocking(move || run_deadline_sweep_tick(&pool_for_sweep))
            .await
            .unwrap();
        let (_, assignments_again) = keyholder
            .get(&format!(
                "/api/v1/keyholder/submissives/{sub_id}/assignments"
            ))
            .await;
        assert_eq!(
            assignments_again
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| a["title"] == "Morning cage photo")
                .count(),
            1
        );

        // Ownership scoping: an unrelated Keyholder can't patch this rule.
        seed_keyholder(
            &pool,
            "kh-recur-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-recur-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other_kh
            .patch(
                &format!("/api/v1/keyholder/recurring-tasks/{rule_id}"),
                serde_json::json!({"active": false}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// `GET /submissive/stats` and `GET /keyholder/submissives/{id}/stats`
    /// (03-api-design.md §15) — both share one aggregation, so a
    /// completed task, an open confinement session, and a bad `period`
    /// exercise the same numbers both roles are meant to see identically.
    #[tokio::test]
    async fn statistics_reflect_activity_identically_for_both_roles() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-stats@example.test",
            "sub-stats@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (_, device) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/devices"),
                serde_json::json!({"name": "steel"}),
            )
            .await;
        let device_id = device["id"].as_str().unwrap().to_string();
        keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/confinement-sessions"),
                serde_json::json!({"device_id": device_id, "started_reason": "voluntary"}),
            )
            .await;

        let (_, task) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/assignments"),
                serde_json::json!({
                    "kind": "task", "title": "Log it", "completion_type": "acknowledge_only",
                    "default_deadline_seconds": 3600
                }),
            )
            .await;
        let task_id = task["id"].as_str().unwrap().to_string();
        submissive
            .patch(
                &format!("/api/v1/submissive/assignments/{task_id}/acknowledge"),
                serde_json::json!({}),
            )
            .await;
        keyholder
            .patch(
                &format!("/api/v1/keyholder/assignments/{task_id}"),
                serde_json::json!({"status": "completed"}),
            )
            .await;

        // Bad period is rejected up front, for both roles.
        let (status, _) = submissive.get("/api/v1/submissive/stats?period=6w").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = keyholder
            .get(&format!(
                "/api/v1/keyholder/submissives/{sub_id}/stats?period=6w"
            ))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, sub_view) = submissive.get("/api/v1/submissive/stats").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(sub_view["period"], "all");
        assert_eq!(sub_view["tasks"]["assigned"], 1);
        assert_eq!(sub_view["tasks"]["completed"], 1);
        assert!(sub_view["current_streak_seconds"].as_i64().unwrap() >= 0);
        assert!(sub_view["lifetime_locked_seconds"].as_i64().unwrap() >= 0);

        // A Keyholder sees exactly the same shape for the same submissive
        // — one shared mental model of "the numbers," not two rollups.
        let (status, kh_view) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/stats"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(kh_view, sub_view);

        // Ownership scoping: an unrelated Keyholder can't see this submissive's stats.
        seed_keyholder(
            &pool,
            "kh-stats-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-stats-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other_kh
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/stats"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// Oversight pause (06-future-extensions.md §13): pausing cascades
    /// into an open confinement session, freezes the deadline
    /// sweeper's auto-fail pass for open tasks, and resuming shifts
    /// those deadlines forward by the elapsed pause length rather than
    /// letting the very next tick auto-fail everything at once.
    #[tokio::test]
    async fn oversight_pause_cascades_freezes_deadlines_and_resume_shifts_them() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, mut submissive, _blob_dir) = linked_keyholder_and_submissive(
            &pool,
            "kh-oversight@example.test",
            "sub-oversight@example.test",
        )
        .await;
        let sub_id = submissive_id(&mut keyholder).await;

        let (_, device) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/devices"),
                serde_json::json!({"name": "steel"}),
            )
            .await;
        let device_id = device["id"].as_str().unwrap().to_string();
        keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/confinement-sessions"),
                serde_json::json!({"device_id": device_id, "started_reason": "voluntary"}),
            )
            .await;

        let (_, task) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/assignments"),
                serde_json::json!({
                    "kind": "task", "title": "Log it", "completion_type": "acknowledge_only",
                    "default_deadline_seconds": 3600
                }),
            )
            .await;
        let task_id = task["id"].as_str().unwrap().to_string();

        // Engage the oversight pause.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/oversight-pause"),
                serde_json::json!({"message": "traveling, back in a week"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // A second pause is a conflict — already paused.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/oversight-pause"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // It cascaded into the confinement session too.
        let (_, status_body) = keyholder
            .get(&format!("/api/v1/keyholder/submissives/{sub_id}/status"))
            .await;
        assert_eq!(status_body["clock_paused"], true);

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let (_, sub_feed) = submissive.get("/api/v1/notifications").await;
        assert!(
            sub_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "oversight.paused")
        );

        // Back-date the task's deadline into the past and run a real
        // sweep tick — it must NOT auto-fail while oversight-paused.
        let original_deadline: i64 = {
            let conn = pool.get().unwrap();
            conn.query_row(
                "SELECT deadline_at FROM assignments WHERE id = ?1",
                rusqlite::params![task_id],
                |row| row.get(0),
            )
            .unwrap()
        };
        {
            let conn = pool.get().unwrap();
            conn.execute(
                "UPDATE assignments SET deadline_at = ?1 WHERE id = ?2",
                rusqlite::params![crate::auth::session::now() - 10, task_id],
            )
            .unwrap();
        }
        let pool_for_sweep = pool.clone();
        tokio::task::spawn_blocking(move || run_deadline_sweep_tick(&pool_for_sweep))
            .await
            .unwrap();

        let (_, task_after_sweep) = keyholder
            .get(&format!("/api/v1/keyholder/assignments/{task_id}"))
            .await;
        assert_eq!(task_after_sweep["status"], "assigned");

        // Update the pause message while still paused.
        let (status, _) = keyholder
            .patch(
                &format!("/api/v1/keyholder/submissives/{sub_id}/oversight-pause-message"),
                serde_json::json!({"message": "still traveling"}),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Resume: the elapsed pause length shifts the still-open deadline forward.
        let (status, resume_body) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/oversight-resume"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resume_body["shifted_assignment_count"], 1);
        let elapsed = resume_body["elapsed_seconds"].as_i64().unwrap();
        assert!(elapsed >= 0);

        let (_, task_after_resume) = keyholder
            .get(&format!("/api/v1/keyholder/assignments/{task_id}"))
            .await;
        let new_deadline = task_after_resume["deadline_at"].as_str().unwrap();
        assert_eq!(
            new_deadline,
            api::iso8601(crate::auth::session::now() - 10 + elapsed)
        );
        let _ = original_deadline;

        // Resuming again is a conflict — nothing is paused any more.
        let (status, _) = keyholder
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/oversight-resume"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let (_, sub_feed) = submissive.get("/api/v1/notifications").await;
        assert!(
            sub_feed
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["type"] == "oversight.resumed")
        );

        // Ownership scoping: an unrelated Keyholder can't touch this link.
        seed_keyholder(
            &pool,
            "kh-oversight-other@example.test",
            "correct horse battery staple",
        );
        let mut other_kh = TestClient::new(pool.clone());
        other_kh.get("/health").await;
        other_kh
            .post(
                "/api/v1/auth/login",
                serde_json::json!({"email": "kh-oversight-other@example.test", "password": "correct horse battery staple"}),
            )
            .await;
        let (status, _) = other_kh
            .post(
                &format!("/api/v1/keyholder/submissives/{sub_id}/oversight-pause"),
                serde_json::json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
