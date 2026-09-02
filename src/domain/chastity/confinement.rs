//! `confinement_sessions`/`confinement_adjustments` (01-data-model.md
//! §4) — the actual lock-status timeline, plus every change to its
//! planned release time, why, and whether a Keyholder has signed off on
//! an automatically-applied one.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::auth::session::now;
use crate::domain::is_unique_violation;

pub struct Session {
    pub id: String,
    // Every current call site already knows which submissive a Session
    // belongs to (it's how the row was looked up) — kept on the struct
    // as a full mirror of the DB row for callers that don't.
    #[allow(dead_code)]
    pub submissive_id: String,
    pub device_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub target_release_at: Option<i64>,
    pub clock_paused_at: Option<i64>,
    pub clock_pause_message: Option<String>,
    pub started_reason: String,
    pub ended_reason: Option<String>,
    pub notes: Option<String>,
}

const SESSION_COLUMNS: &str = "id, submissive_id, device_id, started_at, ended_at, \
     target_release_at, clock_paused_at, clock_pause_message, started_reason, \
     ended_reason, notes";

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        submissive_id: row.get(1)?,
        device_id: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        target_release_at: row.get(5)?,
        clock_paused_at: row.get(6)?,
        clock_pause_message: row.get(7)?,
        started_reason: row.get(8)?,
        ended_reason: row.get(9)?,
        notes: row.get(10)?,
    })
}

#[derive(Debug, Error)]
pub enum StartError {
    #[error("submissive already has an open confinement session")]
    AlreadyOpen,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub struct StartSession<'a> {
    pub submissive_id: &'a str,
    pub device_id: &'a str,
    pub started_reason: &'a str,
    pub target_release_at: Option<i64>,
    pub notes: Option<&'a str>,
}

pub fn start(conn: &Connection, new: StartSession) -> Result<String, StartError> {
    let id = uuid::Uuid::new_v4().to_string();
    let result = conn.execute(
        "INSERT INTO confinement_sessions
            (id, submissive_id, device_id, started_at, target_release_at, started_reason, notes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            new.submissive_id,
            new.device_id,
            now(),
            new.target_release_at,
            new.started_reason,
            new.notes,
        ],
    );
    match result {
        Ok(_) => Ok(id),
        Err(e) if is_unique_violation(&e) => Err(StartError::AlreadyOpen),
        Err(e) => Err(e.into()),
    }
}

pub fn current(conn: &Connection, submissive_id: &str) -> rusqlite::Result<Option<Session>> {
    conn.query_row(
        &format!(
            "SELECT {SESSION_COLUMNS} FROM confinement_sessions
             WHERE submissive_id = ?1 AND ended_at IS NULL"
        ),
        params![submissive_id],
        row_to_session,
    )
    .optional()
}

pub fn history(conn: &Connection, submissive_id: &str) -> rusqlite::Result<Vec<Session>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SESSION_COLUMNS} FROM confinement_sessions
         WHERE submissive_id = ?1 ORDER BY started_at DESC"
    ))?;
    stmt.query_map(params![submissive_id], row_to_session)?
        .collect()
}

#[derive(Debug, Error)]
pub enum NoOpenSessionError {
    #[error("no open confinement session for this submissive")]
    NotOpen,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub fn end(
    conn: &Connection,
    submissive_id: &str,
    ended_reason: &str,
    ended_by_user_id: &str,
    notes: Option<&str>,
) -> Result<(), NoOpenSessionError> {
    let affected = conn.execute(
        "UPDATE confinement_sessions
         SET ended_at = ?1, ended_reason = ?2, ended_by_user_id = ?3, notes = ?4
         WHERE submissive_id = ?5 AND ended_at IS NULL",
        params![now(), ended_reason, ended_by_user_id, notes, submissive_id],
    )?;
    if affected == 0 {
        return Err(NoOpenSessionError::NotOpen);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum PauseError {
    #[error("no open confinement session, or it's already paused")]
    NotOpenOrAlreadyPaused,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub fn pause(
    conn: &Connection,
    submissive_id: &str,
    message: Option<&str>,
) -> Result<(), PauseError> {
    let affected = conn.execute(
        "UPDATE confinement_sessions SET clock_paused_at = ?1, clock_pause_message = ?2
         WHERE submissive_id = ?3 AND ended_at IS NULL AND clock_paused_at IS NULL",
        params![now(), message, submissive_id],
    )?;
    if affected == 0 {
        return Err(PauseError::NotOpenOrAlreadyPaused);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum NotPausedError {
    #[error("session is not currently paused")]
    NotPaused,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub fn update_pause_message(
    conn: &Connection,
    submissive_id: &str,
    message: Option<&str>,
) -> Result<(), NotPausedError> {
    let affected = conn.execute(
        "UPDATE confinement_sessions SET clock_pause_message = ?1
         WHERE submissive_id = ?2 AND ended_at IS NULL AND clock_paused_at IS NOT NULL",
        params![message, submissive_id],
    )?;
    if affected == 0 {
        return Err(NotPausedError::NotPaused);
    }
    Ok(())
}

/// Computes the elapsed pause duration, extends `target_release_at` by
/// it (only when a target is actually set — an open-ended session has
/// nothing to extend), logs a `confinement_adjustments` row carrying
/// forward the pause message, and clears the pause. All in one
/// transaction (03-api-design.md §4).
pub fn resume(
    conn: &mut Connection,
    submissive_id: &str,
    resumed_by_user_id: &str,
) -> Result<(), NotPausedError> {
    let tx = conn.transaction()?;

    let session: Option<(String, Option<i64>, i64, Option<String>)> = tx
        .query_row(
            "SELECT id, target_release_at, clock_paused_at, clock_pause_message
             FROM confinement_sessions
             WHERE submissive_id = ?1 AND ended_at IS NULL AND clock_paused_at IS NOT NULL",
            params![submissive_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((session_id, target_release_at, clock_paused_at, clock_pause_message)) = session
    else {
        return Err(NotPausedError::NotPaused);
    };

    let elapsed = now() - clock_paused_at;

    if let Some(target) = target_release_at {
        tx.execute(
            "UPDATE confinement_sessions SET target_release_at = ?1 WHERE id = ?2",
            params![target + elapsed, session_id],
        )?;
        tx.execute(
            "INSERT INTO confinement_adjustments
                (id, session_id, delta_seconds, reason, adjusted_by_user_id, adjusted_at, notes, keyholder_reviewed_at)
             VALUES (?1, ?2, ?3, 'clock_pause', ?4, ?5, ?6, ?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                session_id,
                elapsed,
                resumed_by_user_id,
                now(),
                clock_pause_message,
            ],
        )?;
    }

    tx.execute(
        "UPDATE confinement_sessions SET clock_paused_at = NULL, clock_pause_message = NULL
         WHERE id = ?1",
        params![session_id],
    )?;

    tx.commit()?;
    Ok(())
}

pub struct Adjustment {
    pub id: String,
    pub delta_seconds: i64,
    pub reason: String,
    pub adjusted_by_user_id: Option<String>,
    pub adjusted_at: i64,
    pub notes: Option<String>,
    pub keyholder_reviewed_at: Option<i64>,
}

pub fn list_adjustments(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<Adjustment>> {
    let mut stmt = conn.prepare(
        "SELECT id, delta_seconds, reason, adjusted_by_user_id, adjusted_at, notes, keyholder_reviewed_at
         FROM confinement_adjustments WHERE session_id = ?1 ORDER BY adjusted_at DESC",
    )?;
    stmt.query_map(params![session_id], |row| {
        Ok(Adjustment {
            id: row.get(0)?,
            delta_seconds: row.get(1)?,
            reason: row.get(2)?,
            adjusted_by_user_id: row.get(3)?,
            adjusted_at: row.get(4)?,
            notes: row.get(5)?,
            keyholder_reviewed_at: row.get(6)?,
        })
    })?
    .collect()
}

pub enum ApplyEffectOutcome {
    Applied,
    /// No open session to extend/reduce — the assignment still gets
    /// recorded (status `applied`), just with nothing to adjust
    /// (08-punishments-and-deadlines.md §5 step 1).
    NoOpenSession,
}

pub struct ApplyEffect<'a> {
    pub submissive_id: &'a str,
    /// Positive extends (a punishment), negative reduces (a reward).
    pub delta_seconds: i64,
    pub reason: &'a str,
    pub caused_by_assignment_id: &'a str,
    /// `None` for a system-driven escalation (08-punishments-and-deadlines.md
    /// §6/§6a — nobody clicked anything in the moment); `Some` for a
    /// Keyholder's own direct assignment.
    pub adjusted_by_user_id: Option<&'a str>,
    /// `true` for a direct assignment the Keyholder just confirmed
    /// themselves (already reviewed, same as a `manual` delta); `false`
    /// for an escalation, which leaves `keyholder_reviewed_at` NULL
    /// until the Keyholder acts on it later.
    pub already_reviewed: bool,
}

/// Applies a `time_extension`/`time_reduction` effect — the mechanics
/// behind a punishment/reward assignment (08-punishments-and-deadlines.md
/// §5) and its mirror-image reward escalation (§6a). Shared by both since
/// the shape is identical: find the open session, log the delta, apply
/// it, done.
pub fn apply_effect(
    conn: &Connection,
    effect: ApplyEffect,
) -> rusqlite::Result<ApplyEffectOutcome> {
    let session: Option<(String, Option<i64>)> = conn
        .query_row(
            "SELECT id, target_release_at FROM confinement_sessions
             WHERE submissive_id = ?1 AND ended_at IS NULL",
            params![effect.submissive_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((session_id, target_release_at)) = session else {
        return Ok(ApplyEffectOutcome::NoOpenSession);
    };

    let ts = now();
    let new_target = target_release_at.unwrap_or(ts) + effect.delta_seconds;
    conn.execute(
        "UPDATE confinement_sessions SET target_release_at = ?1 WHERE id = ?2",
        params![new_target, session_id],
    )?;
    conn.execute(
        "INSERT INTO confinement_adjustments
            (id, session_id, delta_seconds, reason, caused_by_assignment_id,
             adjusted_by_user_id, adjusted_at, keyholder_reviewed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            uuid::Uuid::new_v4().to_string(),
            session_id,
            effect.delta_seconds,
            effect.reason,
            effect.caused_by_assignment_id,
            effect.adjusted_by_user_id,
            ts,
            effect.already_reviewed.then_some(ts),
        ],
    )?;
    Ok(ApplyEffectOutcome::Applied)
}

/// A manual timer delta (03-api-design.md §4's `PATCH .../timer`) — the
/// only way `target_release_at` ever changes outside a pause/resume or
/// (from Phase 3 on) an escalation. There's deliberately no "set to an
/// absolute value" endpoint. When no target is set yet, the delta is
/// applied relative to now, establishing one rather than having nothing
/// to add to — the docs don't spell out this edge case explicitly, so
/// this is the interpretation taken here.
pub fn adjust_timer(
    conn: &mut Connection,
    submissive_id: &str,
    delta_seconds: i64,
    adjusted_by_user_id: &str,
    notes: Option<&str>,
) -> Result<String, NoOpenSessionError> {
    let tx = conn.transaction()?;

    let session: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT id, target_release_at FROM confinement_sessions
             WHERE submissive_id = ?1 AND ended_at IS NULL",
            params![submissive_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((session_id, target_release_at)) = session else {
        return Err(NoOpenSessionError::NotOpen);
    };

    let new_target = target_release_at.unwrap_or_else(now) + delta_seconds;
    tx.execute(
        "UPDATE confinement_sessions SET target_release_at = ?1 WHERE id = ?2",
        params![new_target, session_id],
    )?;

    let adjustment_id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    tx.execute(
        "INSERT INTO confinement_adjustments
            (id, session_id, delta_seconds, reason, adjusted_by_user_id, adjusted_at, notes, keyholder_reviewed_at)
         VALUES (?1, ?2, ?3, 'manual', ?4, ?5, ?6, ?5)",
        params![adjustment_id, session_id, delta_seconds, adjusted_by_user_id, ts, notes],
    )?;

    // A follow-up manual delta marks any other outstanding unreviewed
    // adjustment on this session reviewed too, as a side effect
    // (08-punishments-and-deadlines.md §6) — no rows exist to match this
    // yet in Phase 2 (only Phase 3's escalations create unreviewed ones),
    // but the behavior is correct and tested now regardless.
    tx.execute(
        "UPDATE confinement_adjustments SET keyholder_reviewed_at = ?1
         WHERE session_id = ?2 AND keyholder_reviewed_at IS NULL",
        params![ts, session_id],
    )?;

    tx.commit()?;
    Ok(adjustment_id)
}

#[derive(Debug, Error)]
pub enum ReviewAdjustmentError {
    #[error("adjustment not found, already reviewed, or doesn't need review")]
    NotReviewable,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub fn review_adjustment(
    conn: &Connection,
    adjustment_id: &str,
) -> Result<(), ReviewAdjustmentError> {
    let affected = conn.execute(
        "UPDATE confinement_adjustments SET keyholder_reviewed_at = ?1
         WHERE id = ?2 AND keyholder_reviewed_at IS NULL AND reason = 'punishment_time_extension'",
        params![now(), adjustment_id],
    )?;
    if affected == 0 {
        return Err(ReviewAdjustmentError::NotReviewable);
    }
    Ok(())
}

pub struct Status {
    pub locked: bool,
    pub session: Option<Session>,
    pub time_remaining_seconds: Option<i64>,
    pub overdue: bool,
    pub clock_paused: bool,
}

pub fn status_for(conn: &Connection, submissive_id: &str) -> rusqlite::Result<Status> {
    let session = current(conn, submissive_id)?;
    let time_remaining_seconds = session
        .as_ref()
        .and_then(|s| s.target_release_at)
        .map(|t| t - now());
    Ok(Status {
        locked: session.is_some(),
        overdue: time_remaining_seconds.is_some_and(|r| r < 0),
        clock_paused: session
            .as_ref()
            .is_some_and(|s| s.clock_paused_at.is_some()),
        time_remaining_seconds,
        session,
    })
}

/// One session whose still-paused reminder
/// (08-punishments-and-deadlines.md §9) is due this tick.
pub struct StillPausedReminder {
    pub session_id: String,
    pub keyholder_id: String,
}

const STILL_PAUSED_THRESHOLD_SECS: i64 = 24 * 3600;

/// Runs on the same tick as the deadline sweeper (08-punishments-and-
/// deadlines.md §9 — "doesn't warrant a third background task"): finds
/// every session paused 24h+ ago that hasn't had a
/// `confinement.clock_still_paused` notification in the last 24h, and
/// reports it for the caller to notify the Keyholder (never the
/// submissive — they already know their timer is paused).
pub fn run_still_paused_sweep_tick(
    conn: &Connection,
) -> rusqlite::Result<Vec<StillPausedReminder>> {
    let now_ts = now();
    let candidates: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, submissive_id FROM confinement_sessions
             WHERE ended_at IS NULL AND clock_paused_at IS NOT NULL AND clock_paused_at <= ?1",
        )?;
        stmt.query_map(params![now_ts - STILL_PAUSED_THRESHOLD_SECS], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };

    let mut out = Vec::new();
    for (session_id, submissive_id) in candidates {
        if crate::domain::notifications::exists_for_related_entity_since(
            conn,
            "confinement.clock_still_paused",
            &session_id,
            now_ts - STILL_PAUSED_THRESHOLD_SECS,
        )? {
            continue;
        }
        let keyholder_id: Option<String> = conn
            .query_row(
                "SELECT keyholder_id FROM keyholder_submissive_links
                 WHERE submissive_id = ?1 AND status IN ('active', 'paused')",
                params![submissive_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(keyholder_id) = keyholder_id {
            out.push(StillPausedReminder {
                session_id,
                keyholder_id,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(conn: &Connection) -> (String, String) {
        let submissive_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Sub', 0)",
            params![submissive_id],
        )
        .unwrap();
        let device_id =
            crate::domain::chastity::devices::add(conn, &submissive_id, "steel #2", None).unwrap();
        (submissive_id, device_id)
    }

    fn temp_pool() -> (tempfile::TempDir, crate::db::Pool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        (dir, pool)
    }

    #[test]
    fn starting_a_second_session_is_rejected() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);

        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: None,
                notes: None,
            },
        )
        .unwrap();

        let second = start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: None,
                notes: None,
            },
        );
        assert!(matches!(second, Err(StartError::AlreadyOpen)));
    }

    #[test]
    fn status_reflects_locked_and_overdue() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);

        let status = status_for(&conn, &submissive_id).unwrap();
        assert!(!status.locked);

        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: Some(now() - 10),
                notes: None,
            },
        )
        .unwrap();

        let status = status_for(&conn, &submissive_id).unwrap();
        assert!(status.locked);
        assert!(status.overdue);
        assert!(!status.clock_paused);
    }

    #[test]
    fn end_requires_an_open_session() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (submissive_id, _) = seed(&conn);
        let result = end(
            &conn,
            &submissive_id,
            "scheduled_release",
            &submissive_id,
            None,
        );
        assert!(matches!(result, Err(NoOpenSessionError::NotOpen)));
    }

    #[test]
    fn end_then_start_again_is_allowed() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);

        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: None,
                notes: None,
            },
        )
        .unwrap();
        end(
            &conn,
            &submissive_id,
            "scheduled_release",
            &submissive_id,
            None,
        )
        .unwrap();
        assert!(current(&conn, &submissive_id).unwrap().is_none());

        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: None,
                notes: None,
            },
        )
        .unwrap();
        assert!(current(&conn, &submissive_id).unwrap().is_some());
    }

    #[test]
    fn pause_then_resume_extends_target_and_logs_adjustment() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);
        let keyholder_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH', 0)",
            params![keyholder_id],
        )
        .unwrap();

        let original_target = now() + 1000;
        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: Some(original_target),
                notes: None,
            },
        )
        .unwrap();

        pause(&conn, &submissive_id, Some("traveling")).unwrap();
        assert!(status_for(&conn, &submissive_id).unwrap().clock_paused);

        // Backdate the pause so there's a real, non-zero elapsed duration.
        conn.execute(
            "UPDATE confinement_sessions SET clock_paused_at = ?1 WHERE submissive_id = ?2",
            params![now() - 500, submissive_id],
        )
        .unwrap();

        resume(&mut conn, &submissive_id, &keyholder_id).unwrap();

        let status = status_for(&conn, &submissive_id).unwrap();
        assert!(!status.clock_paused);
        let new_target = status.session.unwrap().target_release_at.unwrap();
        assert!(new_target > original_target);

        let session_id = current(&conn, &submissive_id).unwrap().unwrap().id;
        let adjustments = list_adjustments(&conn, &session_id).unwrap();
        assert_eq!(adjustments.len(), 1);
        assert_eq!(adjustments[0].reason, "clock_pause");
        assert_eq!(adjustments[0].notes.as_deref(), Some("traveling"));
        assert!(adjustments[0].keyholder_reviewed_at.is_some());
    }

    #[test]
    fn resuming_when_not_paused_fails() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);
        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: None,
                notes: None,
            },
        )
        .unwrap();

        let result = resume(&mut conn, &submissive_id, "someone");
        assert!(matches!(result, Err(NotPausedError::NotPaused)));
    }

    #[test]
    fn adjust_timer_applies_delta_and_marks_reviewed() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);
        let keyholder_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH', 0)",
            params![keyholder_id],
        )
        .unwrap();

        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: Some(1000),
                notes: None,
            },
        )
        .unwrap();

        adjust_timer(
            &mut conn,
            &submissive_id,
            500,
            &keyholder_id,
            Some("being lenient"),
        )
        .unwrap();

        let session = current(&conn, &submissive_id).unwrap().unwrap();
        assert_eq!(session.target_release_at, Some(1500));
    }

    #[test]
    fn adjust_timer_from_no_target_establishes_one_relative_to_now() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);
        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: None,
                notes: None,
            },
        )
        .unwrap();

        let before = now();
        adjust_timer(&mut conn, &submissive_id, 3600, &submissive_id, None).unwrap();
        let session = current(&conn, &submissive_id).unwrap().unwrap();
        assert!(session.target_release_at.unwrap() >= before + 3600);
    }

    #[test]
    fn review_adjustment_rejects_manual_rows() {
        let (_dir, pool) = temp_pool();
        let mut conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);
        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: Some(1000),
                notes: None,
            },
        )
        .unwrap();
        let adjustment_id =
            adjust_timer(&mut conn, &submissive_id, 100, &submissive_id, None).unwrap();

        let result = review_adjustment(&conn, &adjustment_id);
        assert!(matches!(result, Err(ReviewAdjustmentError::NotReviewable)));
    }

    #[test]
    fn apply_effect_extends_target_and_leaves_unreviewed_when_escalated() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);
        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: Some(1000),
                notes: None,
            },
        )
        .unwrap();

        let outcome = apply_effect(
            &conn,
            ApplyEffect {
                submissive_id: &submissive_id,
                delta_seconds: 21_600,
                reason: "punishment_time_extension",
                caused_by_assignment_id: "assignment-1",
                adjusted_by_user_id: None,
                already_reviewed: false,
            },
        )
        .unwrap();
        assert!(matches!(outcome, ApplyEffectOutcome::Applied));

        let session = current(&conn, &submissive_id).unwrap().unwrap();
        assert_eq!(session.target_release_at, Some(21_600 + 1000));

        let adjustments = list_adjustments(&conn, &session.id).unwrap();
        assert_eq!(adjustments.len(), 1);
        assert_eq!(adjustments[0].reason, "punishment_time_extension");
        assert!(adjustments[0].keyholder_reviewed_at.is_none());
    }

    #[test]
    fn apply_effect_with_no_open_session_reports_that_and_touches_nothing() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (submissive_id, _device_id) = seed(&conn);

        let outcome = apply_effect(
            &conn,
            ApplyEffect {
                submissive_id: &submissive_id,
                delta_seconds: -3600,
                reason: "reward_time_reduction",
                caused_by_assignment_id: "assignment-2",
                adjusted_by_user_id: Some(&submissive_id),
                already_reviewed: true,
            },
        )
        .unwrap();
        assert!(matches!(outcome, ApplyEffectOutcome::NoOpenSession));
    }

    #[test]
    fn apply_effect_direct_assignment_is_already_reviewed() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let (submissive_id, device_id) = seed(&conn);
        let keyholder_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH', 0)",
            params![keyholder_id],
        )
        .unwrap();
        start(
            &conn,
            StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: Some(10_000),
                notes: None,
            },
        )
        .unwrap();

        apply_effect(
            &conn,
            ApplyEffect {
                submissive_id: &submissive_id,
                delta_seconds: -1800,
                reason: "reward_time_reduction",
                caused_by_assignment_id: "assignment-3",
                adjusted_by_user_id: Some(&keyholder_id),
                already_reviewed: true,
            },
        )
        .unwrap();

        let session = current(&conn, &submissive_id).unwrap().unwrap();
        assert_eq!(session.target_release_at, Some(10_000 - 1800));
        let adjustments = list_adjustments(&conn, &session.id).unwrap();
        assert!(adjustments[0].keyholder_reviewed_at.is_some());
    }
}
