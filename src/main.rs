mod api;
mod auth;
mod db;
mod domain;
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
                .merge(api::chastity::router())
                .merge(api::verification::router())
                .merge(api::proofs::router())
                .merge(api::templates::router())
                .merge(api::assignments::router()),
        )
        .merge(web::router())
        .nest_service("/static", tower_http::services::ServeDir::new("static"))
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

/// One tick of the task-deadline sweeper's auto-fail pass
/// (08-punishments-and-deadlines.md §3 step 1) — same heartbeat
/// discipline as verification issuance. The deadline-approaching
/// reminder pass (§3 step 2) isn't built here since it needs the
/// `notifications` table (Phase 5).
fn run_deadline_sweep_tick(pool: &db::Pool) {
    let mut conn = match pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            tracing::error!(error = %e, "deadline sweep: failed to get a DB connection");
            return;
        }
    };
    match domain::rewards_punishments::assignments::run_deadline_sweep_tick(&mut conn) {
        Ok(failed) => {
            let _ = ops::record_heartbeat(&conn, "deadline_sweeper", true, None, failed);
            if failed > 0 {
                tracing::info!(failed, "tasks auto-failed on deadline");
            }
        }
        Err(e) => {
            let _ =
                ops::record_heartbeat(&conn, "deadline_sweeper", false, Some(&e.to_string()), 0);
            tracing::error!(error = %e, "deadline sweep tick failed");
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
    };
    let app = build_router(state);

    let listen_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    tracing::info!(addr = %listen_addr, "listening");

    axum::serve(listener, app).await?;
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
    if !yes {
        print!("Create a keyholder account for '{email}'? Type the email to confirm: ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim() != email {
            anyhow::bail!("confirmation did not match — aborted, no account created");
        }
    }

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

    fn test_app_state(pool: db::Pool, blob_dir: std::path::PathBuf) -> Router {
        let state = db::AppState {
            pool,
            blob_dir: db::BlobDir(blob_dir),
        };
        build_router(state)
    }

    impl TestClient {
        fn new(pool: db::Pool) -> Self {
            let blob_dir = tempfile::tempdir().unwrap();
            let app = test_app_state(pool, blob_dir.path().to_path_buf());
            Self {
                app,
                cookies: HashMap::new(),
                _blob_dir: Some(blob_dir),
            }
        }

        /// For two `TestClient`s standing in for two different people
        /// hitting the same running server — they must share one blob
        /// dir, the way one real process's `AppState` does, or a file
        /// one of them uploads is invisible to the other.
        fn new_with_blob_dir(pool: db::Pool, blob_dir: &std::path::Path) -> Self {
            let app = test_app_state(pool, blob_dir.to_path_buf());
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

    #[tokio::test]
    async fn mutating_request_without_csrf_is_rejected() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh6@example.test", "correct horse battery staple");
        let blob_dir = tempfile::tempdir().unwrap();
        let app = test_app_state(pool, blob_dir.path().to_path_buf());
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
        seed_keyholder(pool, keyholder_email, "correct horse battery staple");
        let mut keyholder = TestClient::new_with_blob_dir(pool.clone(), blob_dir.path());
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

        let mut submissive = TestClient::new_with_blob_dir(pool.clone(), blob_dir.path());
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
    async fn device_and_confinement_lifecycle() {
        let (_dir, pool) = temp_pool();
        let (mut keyholder, _submissive, _blob_dir) = linked_keyholder_and_submissive(
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
        let (mut keyholder, _submissive, _blob_dir) = linked_keyholder_and_submissive(
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

        let pool_for_sweep = pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool_for_sweep.get().unwrap();
            domain::rewards_punishments::assignments::run_deadline_sweep_tick(&mut conn).unwrap()
        })
        .await
        .unwrap();

        let (_, updated) = keyholder
            .get(&format!("/api/v1/keyholder/assignments/{task_id}"))
            .await;
        assert_eq!(updated["status"], "failed");
    }
}
