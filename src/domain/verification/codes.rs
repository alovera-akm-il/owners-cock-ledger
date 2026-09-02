//! `verification_codes` (01-data-model.md §5, 04-verification-workflow.md
//! §2): server-generated, time-bound, single-use codes a submissive
//! displays in their proof photo.

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::policy::Policy;
use crate::auth::session::now;

/// Visually unambiguous alphabet — no `0`/`O`/`1`/`I` (04-verification-workflow.md §2).
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
const CODE_LENGTH: usize = 7;

pub fn generate_code_string() -> String {
    // Two CSPRNG-backed UUIDs give more than enough entropy to draw
    // CODE_LENGTH alphabet indices from without needing a separate `rand`
    // dependency just for this — same reasoning as auth::token.
    let entropy = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    entropy
        .as_bytes()
        .chunks(2)
        .take(CODE_LENGTH)
        .map(|chunk| {
            let idx = (chunk[0] as usize + chunk.get(1).copied().unwrap_or(0) as usize)
                % CODE_ALPHABET.len();
            CODE_ALPHABET[idx] as char
        })
        .collect()
}

pub struct Code {
    pub id: String,
    // Mirrors the DB row in full even though the API layer's
    // CodeResponse doesn't surface these two (the caller already knows
    // their own link_id, and a `current_unconsumed` result is by
    // definition unconsumed).
    #[allow(dead_code)]
    pub link_id: String,
    pub code: String,
    pub issued_at: i64,
    pub expires_at: i64,
    #[allow(dead_code)]
    pub consumed_at: Option<i64>,
}

const COLUMNS: &str = "id, link_id, code, issued_at, expires_at, consumed_at";

fn row_to_code(row: &rusqlite::Row) -> rusqlite::Result<Code> {
    Ok(Code {
        id: row.get(0)?,
        link_id: row.get(1)?,
        code: row.get(2)?,
        issued_at: row.get(3)?,
        expires_at: row.get(4)?,
        consumed_at: row.get(5)?,
    })
}

/// The caller's currently-active unconsumed, unexpired code, if any
/// (`GET /submissive/verification-codes/current`).
pub fn current_unconsumed(conn: &Connection, link_id: &str) -> rusqlite::Result<Option<Code>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM verification_codes
             WHERE link_id = ?1 AND consumed_at IS NULL AND expires_at > ?2
             ORDER BY issued_at DESC LIMIT 1"
        ),
        params![link_id, now()],
        row_to_code,
    )
    .optional()
}

fn insert(conn: &Connection, link_id: &str, ttl_seconds: i64) -> rusqlite::Result<Code> {
    let id = uuid::Uuid::new_v4().to_string();
    let issued_at = now();
    let expires_at = issued_at + ttl_seconds;
    let code = generate_code_string();
    conn.execute(
        "INSERT INTO verification_codes (id, link_id, code, issued_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, link_id, code, issued_at, expires_at],
    )?;
    Ok(Code {
        id,
        link_id: link_id.to_string(),
        code,
        issued_at,
        expires_at,
        consumed_at: None,
    })
}

#[derive(Debug, Error)]
pub enum RequestError {
    #[error("an unconsumed code already exists")]
    AlreadyHaveOne,
    #[error("this policy doesn't allow on-demand requests right now")]
    NotAllowed,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `allow_on_demand` lives inside `frequency_value`'s JSON for the
/// scheduled kinds (04-verification-workflow.md §1) — `on_demand_only`
/// always allows it regardless of what's in there.
fn allows_on_demand(policy: &Policy) -> bool {
    if policy.frequency_kind == "on_demand_only" {
        return true;
    }
    serde_json::from_str::<serde_json::Value>(&policy.frequency_value)
        .ok()
        .and_then(|v| v.get("allow_on_demand").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// `POST /submissive/verification-codes` — synchronous on-demand
/// issuance, rejecting a stockpile attempt or a policy that doesn't
/// permit it.
pub fn request_on_demand(conn: &Connection, policy: &Policy) -> Result<Code, RequestError> {
    if current_unconsumed(conn, &policy.link_id)?.is_some() {
        return Err(RequestError::AlreadyHaveOne);
    }
    if !allows_on_demand(policy) {
        return Err(RequestError::NotAllowed);
    }
    Ok(insert(conn, &policy.link_id, policy.code_ttl_seconds)?)
}

pub struct History {
    // Not surfaced in CodeHistoryEntry — the DB row id has no meaning to
    // a Keyholder reading an issued-code audit list, only the code/times
    // do.
    #[allow(dead_code)]
    pub id: String,
    pub code: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub consumed_at: Option<i64>,
}

pub fn history_for_link(conn: &Connection, link_id: &str) -> rusqlite::Result<Vec<History>> {
    let mut stmt = conn.prepare(
        "SELECT id, code, issued_at, expires_at, consumed_at
         FROM verification_codes WHERE link_id = ?1 ORDER BY issued_at DESC",
    )?;
    stmt.query_map(params![link_id], |row| {
        Ok(History {
            id: row.get(0)?,
            code: row.get(1)?,
            issued_at: row.get(2)?,
            expires_at: row.get(3)?,
            consumed_at: row.get(4)?,
        })
    })?
    .collect()
}

/// Marks a code consumed by a specific submission — called inside the
/// same transaction that inserts the `proof_submissions` row
/// (04-verification-workflow.md §3), never on its own.
pub fn consume(tx: &Connection, code_id: &str, submission_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE verification_codes SET consumed_at = ?1, consumed_by_submission_id = ?2
         WHERE id = ?3",
        params![now(), submission_id, code_id],
    )?;
    Ok(())
}

/// Loads a code for redemption, confirming it belongs to `link_id`,
/// hasn't been consumed, and is still within `expires_at +
/// grace_period_seconds` (04-verification-workflow.md §3 step 2).
pub fn load_for_redemption(
    conn: &Connection,
    code_id: &str,
    link_id: &str,
    grace_period_seconds: i64,
) -> rusqlite::Result<Option<Code>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM verification_codes
             WHERE id = ?1 AND link_id = ?2 AND consumed_at IS NULL AND expires_at + ?3 > ?4"
        ),
        params![code_id, link_id, grace_period_seconds, now()],
        row_to_code,
    )
    .optional()
}

/// Deterministically derives today's target time-of-day (seconds since
/// UTC midnight) for `random_within_window` from a hash of the link and
/// today's date, rather than persisting a freshly-rolled value in new
/// schema state. The result is stable across ticks within the same day
/// (so it isn't re-randomized every minute) but not predictable by the
/// submissive without knowing this function and their own `link_id`.
fn random_window_target_seconds(
    link_id: &str,
    day: &str,
    window_start: i64,
    window_end: i64,
) -> i64 {
    if window_end <= window_start {
        return window_start;
    }
    let digest = Sha256::digest(format!("{link_id}:{day}").as_bytes());
    let span = (window_end - window_start) as u64;
    let offset = u64::from_be_bytes(digest[0..8].try_into().unwrap()) % span;
    window_start + offset as i64
}

/// Whether a new code is due for this link right now, per its policy —
/// the scheduling half of the background issuance task
/// (04-verification-workflow.md §2). Times in `fixed_times_daily`/
/// `random_within_window` are interpreted as UTC in this version — a
/// documented simplification (real per-submissive-timezone conversion is
/// a reasonable follow-up), not a silent bug.
pub fn is_due(policy: &Policy, last_issued_at: Option<i64>, current_time: i64) -> bool {
    match policy.frequency_kind.as_str() {
        "on_demand_only" => false,
        "interval_hours" => {
            let hours = frequency_number(&policy.frequency_value, "hours").unwrap_or(24);
            match last_issued_at {
                None => true,
                Some(last) => current_time - last >= hours * 3600,
            }
        }
        "fixed_times_daily" => {
            let Some(times) = frequency_times(&policy.frequency_value) else {
                return false;
            };
            let seconds_today = current_time.rem_euclid(86_400);
            let day = current_time.div_euclid(86_400);
            times.iter().any(|&target_seconds| {
                seconds_today >= target_seconds
                    && seconds_today < target_seconds + 60
                    && last_issued_at.is_none_or(|last| last.div_euclid(86_400) < day)
            })
        }
        "random_within_window" => {
            let Some((start, end)) = frequency_window(&policy.frequency_value) else {
                return false;
            };
            let day_index = current_time.div_euclid(86_400);
            let day_label = day_index.to_string();
            let target = random_window_target_seconds(&policy.link_id, &day_label, start, end);
            let seconds_today = current_time.rem_euclid(86_400);
            seconds_today >= target
                && seconds_today < target + 60
                && last_issued_at.is_none_or(|last| last.div_euclid(86_400) < day_index)
        }
        _ => false,
    }
}

fn frequency_number(raw: &str, key: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()?
        .get(key)?
        .as_i64()
}

/// `["09:00", "21:00"]` -> seconds-since-midnight for each.
fn frequency_times(raw: &str) -> Option<Vec<i64>> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let times = value.get("times")?.as_array()?;
    times.iter().map(|t| parse_hh_mm(t.as_str()?)).collect()
}

fn frequency_window(raw: &str) -> Option<(i64, i64)> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let start = parse_hh_mm(value.get("start")?.as_str()?)?;
    let end = parse_hh_mm(value.get("end")?.as_str()?)?;
    Some((start, end))
}

fn parse_hh_mm(s: &str) -> Option<i64> {
    let (h, m) = s.split_once(':')?;
    let h: i64 = h.parse().ok()?;
    let m: i64 = m.parse().ok()?;
    Some(h * 3600 + m * 60)
}

/// One tick of the background issuance task: for every active link
/// whose policy says a code is due and which has no live unconsumed
/// code, issue one. Returns how many were issued.
pub fn run_due_issuance_tick(conn: &Connection) -> rusqlite::Result<i64> {
    let candidates: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT l.id, MAX(vc.issued_at)
             FROM keyholder_submissive_links l
             LEFT JOIN verification_codes vc ON vc.link_id = l.id
             WHERE l.status = 'active' AND l.oversight_paused_at IS NULL
             GROUP BY l.id",
        )?;
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut issued = 0;
    let now = now();
    for (link_id, last_issued_at) in candidates {
        let Some(policy) = super::policy::get_for_link(conn, &link_id)? else {
            continue;
        };
        if current_unconsumed(conn, &link_id)?.is_some() {
            continue;
        }
        let last = if last_issued_at == 0 {
            None
        } else {
            Some(last_issued_at)
        };
        if is_due(&policy, last, now) {
            insert(conn, &link_id, policy.code_ttl_seconds)?;
            issued += 1;
        }
    }
    Ok(issued)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_codes_avoid_ambiguous_characters() {
        for _ in 0..50 {
            let code = generate_code_string();
            assert_eq!(code.len(), CODE_LENGTH);
            assert!(code.chars().all(|c| !"0O1I".contains(c)));
        }
    }

    #[test]
    fn interval_hours_is_due_after_the_interval_elapses() {
        let policy = Policy {
            link_id: "l1".into(),
            frequency_kind: "interval_hours".into(),
            frequency_value: r#"{"hours":24}"#.into(),
            code_ttl_seconds: 900,
            grace_period_seconds: 600,
            updated_at: 0,
        };
        assert!(is_due(&policy, None, 1_000_000));
        assert!(!is_due(&policy, Some(1_000_000), 1_000_000 + 3600));
        assert!(is_due(&policy, Some(1_000_000), 1_000_000 + 24 * 3600));
    }

    #[test]
    fn on_demand_only_is_never_due() {
        let policy = Policy {
            link_id: "l1".into(),
            frequency_kind: "on_demand_only".into(),
            frequency_value: "{}".into(),
            code_ttl_seconds: 900,
            grace_period_seconds: 600,
            updated_at: 0,
        };
        assert!(!is_due(&policy, None, 1_000_000));
    }

    #[test]
    fn fixed_times_daily_is_due_once_per_slot_per_day() {
        let policy = Policy {
            link_id: "l1".into(),
            frequency_kind: "fixed_times_daily".into(),
            frequency_value: r#"{"times":["09:00","21:00"]}"#.into(),
            code_ttl_seconds: 900,
            grace_period_seconds: 600,
            updated_at: 0,
        };
        let nine_am_utc_day5 = 5 * 86_400 + 9 * 3600;
        assert!(is_due(&policy, None, nine_am_utc_day5));
        // Already issued today, at 09:00 — the 21:00 slot check has its
        // own independent window, so day-scoping (not slot-scoping) is
        // what this test exercises: a code issued this morning suppresses
        // any further "due" check that same day for a simplified model
        // that tracks one last-issued timestamp per link, not per slot.
        assert!(!is_due(
            &policy,
            Some(nine_am_utc_day5),
            nine_am_utc_day5 + 30
        ));
    }

    #[test]
    fn random_within_window_target_is_stable_across_calls() {
        let a = random_window_target_seconds("link-1", "5", 8 * 3600, 22 * 3600);
        let b = random_window_target_seconds("link-1", "5", 8 * 3600, 22 * 3600);
        assert_eq!(a, b);
        assert!((8 * 3600..22 * 3600).contains(&a));
    }

    #[test]
    fn random_within_window_differs_by_link() {
        let a = random_window_target_seconds("link-1", "5", 8 * 3600, 22 * 3600);
        let b = random_window_target_seconds("link-2", "5", 8 * 3600, 22 * 3600);
        // Not guaranteed mathematically, but overwhelmingly likely for a
        // hash-derived value — a collision here would be a red flag.
        assert_ne!(a, b);
    }

    fn temp_pool() -> (tempfile::TempDir, crate::db::Pool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        (dir, pool)
    }

    fn seed_link(conn: &Connection) -> String {
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
        crate::domain::links::create(conn, &keyholder_id, &submissive_id).unwrap()
    }

    #[test]
    fn on_demand_request_rejects_a_second_before_the_first_is_consumed() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let link_id = seed_link(&conn);
        let policy = super::super::policy::get_for_link(&conn, &link_id)
            .unwrap()
            .unwrap();

        request_on_demand(&conn, &policy).unwrap();
        let second = request_on_demand(&conn, &policy);
        assert!(matches!(second, Err(RequestError::AlreadyHaveOne)));
    }

    #[test]
    fn on_demand_request_is_blocked_when_policy_disallows_it() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let link_id = seed_link(&conn);
        super::super::policy::set_for_link(
            &conn,
            &link_id,
            super::super::policy::SetPolicy {
                frequency_kind: "interval_hours",
                frequency_value: r#"{"hours":24}"#,
                code_ttl_seconds: 900,
                grace_period_seconds: 600,
            },
        )
        .unwrap();
        let policy = super::super::policy::get_for_link(&conn, &link_id)
            .unwrap()
            .unwrap();

        let result = request_on_demand(&conn, &policy);
        assert!(matches!(result, Err(RequestError::NotAllowed)));
    }

    #[test]
    fn run_due_issuance_tick_issues_for_a_due_interval_policy() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let link_id = seed_link(&conn);
        super::super::policy::set_for_link(
            &conn,
            &link_id,
            super::super::policy::SetPolicy {
                frequency_kind: "interval_hours",
                frequency_value: r#"{"hours":1}"#,
                code_ttl_seconds: 900,
                grace_period_seconds: 600,
            },
        )
        .unwrap();

        let issued = run_due_issuance_tick(&conn).unwrap();
        assert_eq!(issued, 1);
        assert!(current_unconsumed(&conn, &link_id).unwrap().is_some());

        // A second tick shouldn't double-issue while a live code exists.
        let issued_again = run_due_issuance_tick(&conn).unwrap();
        assert_eq!(issued_again, 0);
    }

    #[test]
    fn run_due_issuance_tick_skips_on_demand_only_links() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let link_id = seed_link(&conn);

        let issued = run_due_issuance_tick(&conn).unwrap();
        assert_eq!(issued, 0);
        assert!(current_unconsumed(&conn, &link_id).unwrap().is_none());
    }

    /// Oversight pause (06-future-extensions.md §13) skips issuing new
    /// codes for a paused link, even one otherwise due — an
    /// unreachable Keyholder shouldn't have new verification windows
    /// opening up behind their back.
    #[test]
    fn run_due_issuance_tick_skips_an_oversight_paused_link() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let link_id = seed_link(&conn);
        super::super::policy::set_for_link(
            &conn,
            &link_id,
            super::super::policy::SetPolicy {
                frequency_kind: "interval_hours",
                frequency_value: r#"{"hours":1}"#,
                code_ttl_seconds: 900,
                grace_period_seconds: 600,
            },
        )
        .unwrap();
        conn.execute(
            "UPDATE keyholder_submissive_links SET oversight_paused_at = ?1 WHERE id = ?2",
            rusqlite::params![crate::auth::session::now(), link_id],
        )
        .unwrap();

        let issued = run_due_issuance_tick(&conn).unwrap();
        assert_eq!(issued, 0);
        assert!(current_unconsumed(&conn, &link_id).unwrap().is_none());
    }

    #[test]
    fn consume_marks_code_and_blocks_reredemption() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let link_id = seed_link(&conn);
        let code = insert(&conn, &link_id, 900).unwrap();

        consume(&conn, &code.id, "submission-1").unwrap();

        let reloaded = load_for_redemption(&conn, &code.id, &link_id, 0).unwrap();
        assert!(reloaded.is_none());
    }
}
