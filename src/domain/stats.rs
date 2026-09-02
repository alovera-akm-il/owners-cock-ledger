//! Read-only reporting layer (03-api-design.md §15) — everything here
//! is computed on read from existing tables (`proof_submissions`,
//! `verification_codes`, `assignments`, `confinement_sessions`,
//! `confinement_adjustments`). No new tables, no pre-aggregation: at
//! this app's scale a handful of aggregate queries per request is
//! cheap, and a materialized stats table would solve a performance
//! problem this system doesn't have.

use rusqlite::{Connection, params};

use crate::auth::session::now;

pub struct SessionLengths {
    pub shortest_seconds: i64,
    pub longest_seconds: i64,
    pub average_seconds: i64,
}

pub struct VerificationCounts {
    pub verified: i64,
    pub failed: i64,
    pub missed: i64,
}

pub struct TaskCounts {
    pub assigned: i64,
    pub completed: i64,
    pub failed: i64,
    pub escalated: i64,
}

pub struct TimerAdjustments {
    pub added_seconds: i64,
    pub removed_seconds: i64,
}

pub struct Stats {
    pub period: String,
    pub current_streak_seconds: i64,
    pub personal_best_streak_seconds: i64,
    pub consistency_pct: i64,
    pub session_lengths: SessionLengths,
    pub verification: VerificationCounts,
    pub tasks: TaskCounts,
    pub rewards_given: i64,
    pub punishments_given: i64,
    pub timer_adjustments: TimerAdjustments,
    pub lifetime_locked_seconds: i64,
}

pub fn valid_period(period: &str) -> bool {
    matches!(period, "all" | "30d" | "90d" | "365d")
}

fn window_seconds(period: &str) -> Option<i64> {
    match period {
        "30d" => Some(30 * 86_400),
        "90d" => Some(90 * 86_400),
        "365d" => Some(365 * 86_400),
        _ => None,
    }
}

/// `period=all`'s window starts at the link's `started_at` — a
/// personal-best-style question ("how consistent has this
/// relationship been") rather than one about the account's age.
pub fn compute(
    conn: &Connection,
    link_id: &str,
    submissive_id: &str,
    period: &str,
) -> rusqlite::Result<Stats> {
    let now_ts = now();
    let link_started_at: i64 = conn.query_row(
        "SELECT started_at FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| row.get(0),
    )?;
    let since = match window_seconds(period) {
        Some(secs) => now_ts - secs,
        None => link_started_at,
    };

    let mut stmt = conn.prepare(
        "SELECT started_at, ended_at FROM confinement_sessions WHERE submissive_id = ?1",
    )?;
    let sessions: Vec<(i64, Option<i64>)> = stmt
        .query_map(params![submissive_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;

    let mut lifetime_locked_seconds = 0i64;
    let mut personal_best_streak_seconds = 0i64;
    let mut current_streak_seconds = 0i64;
    let mut locked_within_period = 0i64;
    let mut lengths_in_period: Vec<i64> = Vec::new();

    for (started_at, ended_at) in &sessions {
        let end = ended_at.unwrap_or(now_ts);
        let duration = end - started_at;
        lifetime_locked_seconds += duration;
        personal_best_streak_seconds = personal_best_streak_seconds.max(duration);

        let overlap_start = (*started_at).max(since);
        let overlap_end = end.min(now_ts);
        if overlap_end > overlap_start {
            locked_within_period += overlap_end - overlap_start;
        }

        match ended_at {
            None => current_streak_seconds = now_ts - started_at,
            Some(ended_at) if *ended_at >= since && *ended_at <= now_ts => {
                lengths_in_period.push(duration);
            }
            Some(_) => {}
        }
    }

    let denominator = (now_ts - since).max(1);
    let consistency_pct =
        ((locked_within_period as f64 / denominator as f64) * 100.0).round() as i64;

    let session_lengths = if lengths_in_period.is_empty() {
        SessionLengths {
            shortest_seconds: 0,
            longest_seconds: 0,
            average_seconds: 0,
        }
    } else {
        let sum: i64 = lengths_in_period.iter().sum();
        SessionLengths {
            shortest_seconds: *lengths_in_period.iter().min().unwrap(),
            longest_seconds: *lengths_in_period.iter().max().unwrap(),
            average_seconds: sum / lengths_in_period.len() as i64,
        }
    };

    let verified: i64 = conn.query_row(
        "SELECT COUNT(*) FROM proof_submissions
         WHERE link_id = ?1 AND purpose = 'verification' AND status = 'verified' AND submitted_at >= ?2",
        params![link_id, since],
        |row| row.get(0),
    )?;
    let verification_failed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM proof_submissions
         WHERE link_id = ?1 AND purpose = 'verification' AND status = 'failed' AND submitted_at >= ?2",
        params![link_id, since],
        |row| row.get(0),
    )?;
    let missed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM verification_codes
         WHERE link_id = ?1 AND consumed_at IS NULL AND expires_at < ?2 AND issued_at >= ?3",
        params![link_id, now_ts, since],
        |row| row.get(0),
    )?;

    let tasks_assigned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assignments WHERE link_id = ?1 AND kind = 'task' AND assigned_at >= ?2",
        params![link_id, since],
        |row| row.get(0),
    )?;
    let tasks_completed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assignments
         WHERE link_id = ?1 AND kind = 'task' AND status = 'completed' AND assigned_at >= ?2",
        params![link_id, since],
        |row| row.get(0),
    )?;
    let tasks_failed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assignments
         WHERE link_id = ?1 AND kind = 'task' AND status = 'failed' AND assigned_at >= ?2",
        params![link_id, since],
        |row| row.get(0),
    )?;
    let tasks_escalated: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assignments a
         WHERE a.link_id = ?1 AND a.kind = 'task' AND a.status = 'failed' AND a.assigned_at >= ?2
           AND EXISTS (SELECT 1 FROM assignments c WHERE c.escalated_from_assignment_id = a.id)",
        params![link_id, since],
        |row| row.get(0),
    )?;

    let rewards_given: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assignments
         WHERE link_id = ?1 AND kind = 'reward' AND status != 'revoked' AND assigned_at >= ?2",
        params![link_id, since],
        |row| row.get(0),
    )?;
    let punishments_given: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assignments
         WHERE link_id = ?1 AND kind = 'punishment' AND status != 'revoked' AND assigned_at >= ?2",
        params![link_id, since],
        |row| row.get(0),
    )?;

    let added_seconds: i64 = conn.query_row(
        "SELECT COALESCE(SUM(ca.delta_seconds), 0) FROM confinement_adjustments ca
         JOIN confinement_sessions cs ON cs.id = ca.session_id
         WHERE cs.submissive_id = ?1 AND ca.delta_seconds > 0 AND ca.adjusted_at >= ?2",
        params![submissive_id, since],
        |row| row.get(0),
    )?;
    let removed_seconds: i64 = conn.query_row(
        "SELECT COALESCE(SUM(-ca.delta_seconds), 0) FROM confinement_adjustments ca
         JOIN confinement_sessions cs ON cs.id = ca.session_id
         WHERE cs.submissive_id = ?1 AND ca.delta_seconds < 0 AND ca.adjusted_at >= ?2",
        params![submissive_id, since],
        |row| row.get(0),
    )?;

    Ok(Stats {
        period: period.to_string(),
        current_streak_seconds,
        personal_best_streak_seconds,
        consistency_pct,
        session_lengths,
        verification: VerificationCounts {
            verified,
            failed: verification_failed,
            missed,
        },
        tasks: TaskCounts {
            assigned: tasks_assigned,
            completed: tasks_completed,
            failed: tasks_failed,
            escalated: tasks_escalated,
        },
        rewards_given,
        punishments_given,
        timer_adjustments: TimerAdjustments {
            added_seconds,
            removed_seconds,
        },
        lifetime_locked_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::links;

    fn temp_pool_with_link() -> (tempfile::TempDir, crate::db::Pool, String, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let keyholder_id = uuid::Uuid::new_v4().to_string();
        let submissive_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH', 0)",
            params![keyholder_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Sub', 0)",
            params![submissive_id],
        )
        .unwrap();
        let link_id = links::create(&conn, &keyholder_id, &submissive_id).unwrap();
        (dir, pool, keyholder_id, submissive_id, link_id)
    }

    fn insert_device(conn: &Connection, submissive_id: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO chastity_devices (id, submissive_id, name, added_at) VALUES (?1, ?2, 'Device', 0)",
            params![id, submissive_id],
        )
        .unwrap();
        id
    }

    fn insert_session(
        conn: &Connection,
        submissive_id: &str,
        device_id: &str,
        started_at: i64,
        ended_at: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO confinement_sessions (id, submissive_id, device_id, started_at, ended_at, started_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, 'voluntary')",
            params![uuid::Uuid::new_v4().to_string(), submissive_id, device_id, started_at, ended_at],
        )
        .unwrap();
    }

    fn insert_assignment(
        conn: &Connection,
        link_id: &str,
        kind: &str,
        status: &str,
        effect_kind: Option<&str>,
        escalated_from: Option<&str>,
        assigned_at: i64,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO assignments
                (id, link_id, kind, title, effect_kind, escalated_from_assignment_id,
                 assigned_at, assigned_via, status)
             VALUES (?1, ?2, ?3, 'Title', ?4, ?5, ?6, 'session', ?7)",
            params![
                id,
                link_id,
                kind,
                effect_kind,
                escalated_from,
                assigned_at,
                status
            ],
        )
        .unwrap();
        id
    }

    #[test]
    fn compute_counts_a_completed_session_within_the_period_only() {
        let (_dir, pool, _kh, sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        let device_id = insert_device(&conn, &sub);

        let far_past = now() - 200 * 86_400;
        insert_session(&conn, &sub, &device_id, far_past, Some(far_past + 3600));

        let recent_start = now() - 3600;
        insert_session(&conn, &sub, &device_id, recent_start, Some(now() - 1800));

        let stats = compute(&conn, &link_id, &sub, "30d").unwrap();
        assert_eq!(stats.session_lengths.shortest_seconds, 1800);
        assert_eq!(stats.session_lengths.longest_seconds, 1800);
        assert_eq!(stats.lifetime_locked_seconds, 3600 + 1800);

        let stats_all = compute(&conn, &link_id, &sub, "all").unwrap();
        assert_eq!(stats_all.personal_best_streak_seconds, 3600);
    }

    #[test]
    fn compute_reports_an_open_session_as_the_current_streak() {
        let (_dir, pool, _kh, sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        let device_id = insert_device(&conn, &sub);
        insert_session(&conn, &sub, &device_id, now() - 500, None);

        let stats = compute(&conn, &link_id, &sub, "all").unwrap();
        assert!(stats.current_streak_seconds >= 500);
        assert_eq!(stats.session_lengths.shortest_seconds, 0);
    }

    #[test]
    fn compute_counts_tasks_and_escalation() {
        let (_dir, pool, _kh, _sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();

        let failing_task = insert_assignment(&conn, &link_id, "task", "failed", None, None, now());
        insert_assignment(
            &conn,
            &link_id,
            "punishment",
            "applied",
            Some("time_extension"),
            Some(&failing_task),
            now(),
        );

        let stats = compute(&conn, &link_id, &_sub, "all").unwrap();
        assert_eq!(stats.tasks.assigned, 1);
        assert_eq!(stats.tasks.failed, 1);
        assert_eq!(stats.tasks.escalated, 1);
        assert_eq!(stats.punishments_given, 1);
    }

    #[test]
    fn compute_counts_missed_and_verified_codes() {
        let (_dir, pool, _kh, _sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();

        conn.execute(
            "INSERT INTO verification_codes (id, link_id, code, issued_at, expires_at, consumed_at)
             VALUES ('missed1', ?1, '000000', ?2, ?3, NULL)",
            params![link_id, now() - 3600, now() - 3000],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO verification_codes (id, link_id, code, issued_at, expires_at, consumed_at)
             VALUES ('pending1', ?1, '111111', ?2, ?3, NULL)",
            params![link_id, now() - 60, now() + 600],
        )
        .unwrap();

        let stats = compute(&conn, &link_id, &_sub, "30d").unwrap();
        assert_eq!(stats.verification.missed, 1);
    }
}
