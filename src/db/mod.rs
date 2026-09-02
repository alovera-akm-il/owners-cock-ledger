//! Connection pool + migration runner against `<data-dir>/db.sqlite3`
//! (07-tech-stack.md §2).

use std::path::{Path, PathBuf};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

pub type Pool = r2d2::Pool<SqliteConnectionManager>;

/// The blob directory (`<data-dir>/blobs/`), threaded through the router
/// state as a distinct `FromRef` target from `Pool` — see `AppState`.
#[derive(Clone)]
pub struct BlobDir(pub PathBuf);

/// Router state now carries two independent pieces (the DB pool and the
/// blob directory), extracted separately by handlers via
/// `State<Pool>`/`State<BlobDir>` — axum's `FromRef` pattern for
/// multi-piece state, rather than every handler needing the whole struct.
#[derive(Clone)]
pub struct AppState {
    pub pool: Pool,
    pub blob_dir: BlobDir,
}

impl axum::extract::FromRef<AppState> for Pool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

impl axum::extract::FromRef<AppState> for BlobDir {
    fn from_ref(state: &AppState) -> Self {
        state.blob_dir.clone()
    }
}

/// Numbered, compile-time-embedded `.sql` files under `migrations/`,
/// applied in order. Embedding via `include_str!` at compile time (rather
/// than reading the directory at runtime, `rusqlite_migration`'s
/// `from-directory` feature) keeps the deployed artifact a single binary
/// with no separate `migrations/` folder to ship alongside it, and adds no
/// build-time database dependency — this is just bundling text.
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("../../migrations/0001_users.sql")),
        M::up(include_str!(
            "../../migrations/0002_keyholder_submissive_links.sql"
        )),
        M::up(include_str!("../../migrations/0003_sessions.sql")),
        M::up(include_str!("../../migrations/0004_audit_log.sql")),
        M::up(include_str!("../../migrations/0005_safety_alerts.sql")),
        M::up(include_str!(
            "../../migrations/0006_background_task_runs.sql"
        )),
        M::up(include_str!("../../migrations/0007_profiles.sql")),
        M::up(include_str!("../../migrations/0008_invites.sql")),
        M::up(include_str!(
            "../../migrations/0009_verification_policies.sql"
        )),
        M::up(include_str!("../../migrations/0010_chastity_devices.sql")),
        M::up(include_str!(
            "../../migrations/0011_confinement_sessions.sql"
        )),
        M::up(include_str!(
            "../../migrations/0012_confinement_adjustments.sql"
        )),
        M::up(include_str!("../../migrations/0013_verification_codes.sql")),
        M::up(include_str!("../../migrations/0014_proof_submissions.sql")),
        M::up(include_str!("../../migrations/0015_proof_attachments.sql")),
        M::up(include_str!(
            "../../migrations/0016_reward_punishment_templates.sql"
        )),
        M::up(include_str!("../../migrations/0017_assignments.sql")),
        M::up(include_str!(
            "../../migrations/0018_proof_submissions_voice_and_assignment_fk.sql"
        )),
        M::up(include_str!(
            "../../migrations/0019_confinement_adjustments_reward_reason.sql"
        )),
        M::up(include_str!(
            "../../migrations/0020_password_reset_tokens.sql"
        )),
        M::up(include_str!("../../migrations/0021_two_factor.sql")),
        M::up(include_str!("../../migrations/0022_api_tokens.sql")),
        M::up(include_str!("../../migrations/0023_link_settings.sql")),
        M::up(include_str!("../../migrations/0024_notifications.sql")),
        M::up(include_str!("../../migrations/0025_toys.sql")),
        M::up(include_str!("../../migrations/0026_points.sql")),
    ])
}

fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;",
    )
}

/// Opens the pool and applies pending migrations against `db_path`,
/// creating the file if it doesn't exist yet. Migrations run once, on a
/// dedicated connection, before the pool is handed out for general use.
pub fn init(db_path: &Path) -> anyhow::Result<Pool> {
    let mut setup_conn = Connection::open(db_path)?;
    apply_pragmas(&setup_conn)?;
    migrations().to_latest(&mut setup_conn)?;
    drop(setup_conn);

    let manager = SqliteConnectionManager::file(db_path).with_init(|conn| apply_pragmas(conn));
    let pool = r2d2::Pool::builder().build(manager)?;
    Ok(pool)
}

/// The default data root, `~/.config/<app-name>/`, unless overridden by
/// `DATA_DIR` (07-tech-stack.md §2). Resolved via the `directories` crate
/// rather than hand-rolled `$HOME` string-building.
pub fn resolve_data_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let dirs = directories::ProjectDirs::from("", "", "owners-cock-ledger")
        .ok_or_else(|| anyhow::anyhow!("could not resolve a home directory for this user"))?;
    Ok(dirs.config_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool() -> (tempfile::TempDir, Pool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = init(&dir.path().join("db.sqlite3")).unwrap();
        (dir, pool)
    }

    #[test]
    fn migrations_apply_cleanly() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let table_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'users'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);
    }

    #[test]
    fn foreign_keys_and_wal_are_enabled() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");
    }

    #[test]
    fn foreign_key_violation_is_rejected() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let result = conn.execute(
            "INSERT INTO sessions (id, user_id, created_at, last_seen_at, expires_at)
             VALUES ('s1', 'nonexistent-user', 0, 0, 0)",
            [],
        );
        assert!(result.is_err());
    }
}
