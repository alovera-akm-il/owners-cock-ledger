mod api;
mod auth;
mod db;
mod domain;
mod ops;
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

fn build_router(pool: db::Pool) -> Router {
    Router::new()
        .route("/health", get(ops::health))
        .nest(
            "/api/v1",
            api::auth::router()
                .merge(api::invites::router())
                .merge(api::roster::router()),
        )
        .merge(web::router())
        .nest_service("/static", tower_http::services::ServeDir::new("static"))
        .layer(axum::middleware::from_fn(auth::csrf::csrf_protect))
        .with_state(pool)
}

async fn serve(pool: db::Pool) -> anyhow::Result<()> {
    let app = build_router(pool);

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
    }

    impl TestClient {
        fn new(pool: db::Pool) -> Self {
            Self {
                app: build_router(pool),
                cookies: HashMap::new(),
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

            for value in response.headers().get_all(header::SET_COOKIE) {
                let raw = value.to_str().unwrap();
                if let Some((k, v)) = raw.split(';').next().and_then(|kv| kv.split_once('=')) {
                    self.cookies
                        .insert(k.trim().to_string(), v.trim().to_string());
                }
            }

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
    async fn mutating_request_without_csrf_is_rejected() {
        let (_dir, pool) = temp_pool();
        seed_keyholder(&pool, "kh6@example.test", "correct horse battery staple");
        let app = build_router(pool);
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
}
