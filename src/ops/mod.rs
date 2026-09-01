//! `GET /health` and the background-task heartbeat helper
//! (10-operations.md §3, 03-api-design.md §14).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use rusqlite::params;
use serde::Serialize;

use crate::db::Pool;

/// Expected tick interval for each background task, used to decide how
/// stale a `last_run_at` is allowed to be before the task counts as
/// unhealthy (`healthy = last_run_at within 3x this interval`,
/// 10-operations.md §3). Neither task exists yet as of Phase 0
/// (verification issuance is Phase 2, the deadline sweeper is Phase 3) —
/// this table is here so the health endpoint doesn't need to change shape
/// once they do; a task with no row yet simply doesn't appear in the
/// response.
fn expected_tick_interval_secs(task_name: &str) -> u64 {
    match task_name {
        "verification_issuance" | "deadline_sweeper" => 60,
        _ => 60,
    }
}

#[derive(Serialize)]
struct TaskHealth {
    last_run_at: i64,
    healthy: bool,
}

#[derive(Serialize)]
pub struct HealthResponse {
    status: &'static str,
    db: &'static str,
    background_tasks: std::collections::BTreeMap<String, TaskHealth>,
}

/// Upserts one heartbeat row at the end of a background task's tick
/// (01-data-model.md §11) — not used by anything yet in Phase 0, but
/// exists now so Phase 2/3's tasks have it ready rather than needing to
/// invent it alongside their own first use.
#[allow(dead_code)]
pub fn record_heartbeat(
    conn: &rusqlite::Connection,
    task_name: &str,
    ok: bool,
    error: Option<&str>,
    rows_processed: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO background_task_runs (task_name, last_run_at, last_run_ok, last_error, rows_processed)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(task_name) DO UPDATE SET
            last_run_at = excluded.last_run_at,
            last_run_ok = excluded.last_run_ok,
            last_error = excluded.last_error,
            rows_processed = excluded.rows_processed",
        params![task_name, crate::auth::session::now(), ok, error, rows_processed],
    )?;
    Ok(())
}

pub async fn health(State(pool): State<Pool>) -> (StatusCode, Json<HealthResponse>) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = pool.get()?;
        conn.query_row("SELECT 1", [], |_| Ok(()))?;

        let mut stmt = conn.prepare("SELECT task_name, last_run_at FROM background_task_runs")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await;

    let now = crate::auth::session::now();

    let Ok(Ok(rows)) = result else {
        let body = HealthResponse {
            status: "degraded",
            db: "error",
            background_tasks: Default::default(),
        };
        return (StatusCode::SERVICE_UNAVAILABLE, Json(body));
    };

    let mut background_tasks = std::collections::BTreeMap::new();
    let mut any_unhealthy = false;
    for (task_name, last_run_at) in rows {
        let threshold = expected_tick_interval_secs(&task_name) as i64 * 3;
        let healthy = now - last_run_at <= threshold;
        any_unhealthy |= !healthy;
        background_tasks.insert(
            task_name,
            TaskHealth {
                last_run_at,
                healthy,
            },
        );
    }

    let status = if any_unhealthy { "degraded" } else { "ok" };
    let code = if any_unhealthy {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    (
        code,
        Json(HealthResponse {
            status,
            db: "ok",
            background_tasks,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use tower::ServiceExt;

    fn app(pool: Pool) -> Router {
        Router::new().route("/health", get(health)).with_state(pool)
    }

    #[tokio::test]
    async fn healthy_with_no_background_tasks_registered_yet() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();

        let response = app(pool)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["db"], "ok");
        assert_eq!(parsed["background_tasks"], serde_json::json!({}));
    }

    #[tokio::test]
    async fn degraded_when_a_task_heartbeat_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        {
            let conn = pool.get().unwrap();
            record_heartbeat(&conn, "deadline_sweeper", true, None, 0).unwrap();
            conn.execute(
                "UPDATE background_task_runs SET last_run_at = 0 WHERE task_name = 'deadline_sweeper'",
                [],
            )
            .unwrap();
        }

        let response = app(pool)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["status"], "degraded");
        assert_eq!(
            parsed["background_tasks"]["deadline_sweeper"]["healthy"],
            false
        );
    }

    #[tokio::test]
    async fn healthy_when_a_task_reported_recently() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        {
            let conn = pool.get().unwrap();
            record_heartbeat(&conn, "deadline_sweeper", true, None, 3).unwrap();
        }

        let response = app(pool)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
