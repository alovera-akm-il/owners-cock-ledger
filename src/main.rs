mod auth;
mod db;
mod domain;
mod ops;

use axum::Router;
use axum::routing::get;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let data_dir = db::resolve_data_dir()?;
    std::fs::create_dir_all(&data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    tracing::info!(path = %data_dir.display(), "using data directory");

    let db_path = data_dir.join("db.sqlite3");
    let pool = db::init(&db_path)?;
    tracing::info!("migrations applied");

    let app = Router::new()
        .route("/health", get(ops::health))
        .layer(axum::middleware::from_fn(auth::csrf::csrf_protect))
        .with_state(pool);

    let listen_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    tracing::info!(addr = %listen_addr, "listening");

    axum::serve(listener, app).await?;
    Ok(())
}
