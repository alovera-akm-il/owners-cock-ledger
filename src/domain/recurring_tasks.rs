//! Repeating tasks (06-future-extensions.md §14) — a rule that
//! periodically spawns an ordinary `assignments` row from an existing
//! `kind='task'` template. Not a new task state machine: a spawned
//! assignment is indistinguishable from a manually-assigned one except
//! for `spawned_by_recurring_task_rule_id`, so deadlines, proof
//! review, points, and notifications all keep working unmodified.

use chrono::{Datelike, TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::links;
use crate::domain::rewards_punishments::{assignments, templates};

pub struct Rule {
    pub id: String,
    pub link_id: String,
    pub template_id: String,
    pub recurrence_kind: String,
    pub recurrence_value: String,
    pub allow_overlap: bool,
    pub active: bool,
    pub next_due_at: i64,
    #[allow(dead_code)]
    pub created_by_user_id: String,
    pub created_at: i64,
}

const COLUMNS: &str = "id, link_id, template_id, recurrence_kind, recurrence_value, \
     allow_overlap, active, next_due_at, created_by_user_id, created_at";

fn row_to_rule(row: &rusqlite::Row) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: row.get(0)?,
        link_id: row.get(1)?,
        template_id: row.get(2)?,
        recurrence_kind: row.get(3)?,
        recurrence_value: row.get(4)?,
        allow_overlap: row.get(5)?,
        active: row.get(6)?,
        next_due_at: row.get(7)?,
        created_by_user_id: row.get(8)?,
        created_at: row.get(9)?,
    })
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Rule>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM recurring_task_rules WHERE id = ?1"),
        params![id],
        row_to_rule,
    )
    .optional()
}

pub fn list_for_link(conn: &Connection, link_id: &str) -> rusqlite::Result<Vec<Rule>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM recurring_task_rules WHERE link_id = ?1 ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![link_id], row_to_rule)?.collect()
}

fn seconds_since_midnight(hh_mm: &str) -> Option<i64> {
    let (h, m) = hh_mm.split_once(':')?;
    Some(h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60)
}

/// `0` = Monday .. `6` = Sunday, matching `chrono::Weekday::num_days_from_monday`.
fn weekday_index(day: &str) -> Option<u32> {
    Some(match day {
        "mon" => 0,
        "tue" => 1,
        "wed" => 2,
        "thu" => 3,
        "fri" => 4,
        "sat" => 5,
        "sun" => 6,
        _ => return None,
    })
}

/// The next timestamp `>= from` satisfying this rule's schedule, or
/// `None` if `recurrence_value` doesn't parse for `recurrence_kind`.
/// Times are interpreted as UTC in this version — the same disclosed
/// simplification `verification::codes` already carries for
/// `fixed_times_daily`/`random_within_window`.
fn next_occurrence(kind: &str, value: &str, from: i64) -> Option<i64> {
    let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
    match kind {
        "interval_hours" => {
            let hours = parsed.get("hours")?.as_i64()?;
            if hours <= 0 {
                return None;
            }
            Some(from + hours * 3600)
        }
        "daily" => {
            let target = seconds_since_midnight(parsed.get("time")?.as_str()?)?;
            let day_start = from - from.rem_euclid(86_400);
            let today = day_start + target;
            Some(if today >= from { today } else { today + 86_400 })
        }
        "weekly_days" => {
            let target = seconds_since_midnight(parsed.get("time")?.as_str()?)?;
            let days: Vec<u32> = parsed
                .get("days")?
                .as_array()?
                .iter()
                .filter_map(|d| weekday_index(d.as_str()?))
                .collect();
            if days.is_empty() {
                return None;
            }
            let day_start = from - from.rem_euclid(86_400);
            // A full week plus one extra day guarantees a match even
            // when today's own slot already passed.
            (0..8).find_map(|offset| {
                let candidate = day_start + offset * 86_400 + target;
                if candidate < from {
                    return None;
                }
                let weekday = Utc
                    .timestamp_opt(candidate, 0)
                    .single()?
                    .weekday()
                    .num_days_from_monday();
                days.contains(&weekday).then_some(candidate)
            })
        }
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    #[error("template not found, or not this keyholder's own")]
    TemplateNotFound,
    #[error("the template must be a task, not a reward or punishment")]
    NotATaskTemplate,
    #[error("invalid recurrence_kind or recurrence_value")]
    InvalidRecurrence,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub struct NewRule<'a> {
    pub link_id: &'a str,
    pub keyholder_id: &'a str,
    pub template_id: &'a str,
    pub recurrence_kind: &'a str,
    pub recurrence_value: &'a str,
    pub allow_overlap: bool,
}

/// `POST /keyholder/submissives/{id}/recurring-tasks`.
pub fn create(conn: &Connection, new: NewRule) -> Result<String, RuleError> {
    let template = templates::get(conn, new.template_id)?.ok_or(RuleError::TemplateNotFound)?;
    if template.keyholder_id != new.keyholder_id {
        return Err(RuleError::TemplateNotFound);
    }
    if template.kind != "task" {
        return Err(RuleError::NotATaskTemplate);
    }
    let now = crate::auth::session::now();
    let next_due_at = next_occurrence(new.recurrence_kind, new.recurrence_value, now)
        .ok_or(RuleError::InvalidRecurrence)?;

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO recurring_task_rules
            (id, link_id, template_id, recurrence_kind, recurrence_value, allow_overlap,
             active, next_due_at, created_by_user_id, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,1,?7,?8,?9)",
        params![
            id,
            new.link_id,
            new.template_id,
            new.recurrence_kind,
            new.recurrence_value,
            new.allow_overlap,
            next_due_at,
            new.keyholder_id,
            now,
        ],
    )?;
    Ok(id)
}

#[derive(Default)]
pub struct RuleEdit<'a> {
    pub recurrence_kind: Option<&'a str>,
    pub recurrence_value: Option<&'a str>,
    pub allow_overlap: Option<bool>,
    pub active: Option<bool>,
}

/// `PATCH /keyholder/recurring-tasks/{id}` — changing the schedule
/// recomputes `next_due_at` from now, rather than trying to preserve
/// a fractional old cycle. Returns `false` if no such rule belongs to
/// this Keyholder's link.
pub fn update(
    conn: &Connection,
    id: &str,
    link_id: &str,
    edit: RuleEdit,
) -> Result<bool, RuleError> {
    let Some(current) = get(conn, id)? else {
        return Ok(false);
    };
    if current.link_id != link_id {
        return Ok(false);
    }
    let recurrence_kind = edit.recurrence_kind.unwrap_or(&current.recurrence_kind);
    let recurrence_value = edit.recurrence_value.unwrap_or(&current.recurrence_value);
    let allow_overlap = edit.allow_overlap.unwrap_or(current.allow_overlap);
    let active = edit.active.unwrap_or(current.active);

    let next_due_at = if edit.recurrence_kind.is_some() || edit.recurrence_value.is_some() {
        next_occurrence(
            recurrence_kind,
            recurrence_value,
            crate::auth::session::now(),
        )
        .ok_or(RuleError::InvalidRecurrence)?
    } else {
        current.next_due_at
    };

    conn.execute(
        "UPDATE recurring_task_rules
         SET recurrence_kind = ?1, recurrence_value = ?2, allow_overlap = ?3, active = ?4, next_due_at = ?5
         WHERE id = ?6",
        params![
            recurrence_kind,
            recurrence_value,
            allow_overlap,
            active,
            next_due_at,
            id
        ],
    )?;
    Ok(true)
}

pub struct SpawnedTask {
    pub keyholder_id: String,
    pub submissive_id: String,
    // The caller's notification helper (`notify_for_assignment`)
    // reads `assignment.link_id` directly rather than this field —
    // kept for completeness/future callers that don't already have
    // the assignment in hand.
    #[allow(dead_code)]
    pub link_id: String,
    pub assignment: assignments::Assignment,
}

/// Runs on the same tick as the deadline sweeper
/// (08-punishments-and-deadlines.md §9's reasoning for not wanting a
/// separate background task): for every `active` rule with
/// `next_due_at <= now`, spawns an ordinary task assignment from its
/// template — unless one it already spawned is still open and
/// `allow_overlap` isn't set, in which case this tick is skipped
/// entirely (including leaving `next_due_at` untouched) so the next
/// tick re-checks once the open one resolves.
pub fn run_recurring_task_sweep_tick(conn: &mut Connection) -> rusqlite::Result<Vec<SpawnedTask>> {
    let now_ts = crate::auth::session::now();
    let due: Vec<Rule> = {
        let sql = format!(
            "SELECT {COLUMNS} FROM recurring_task_rules WHERE active = 1 AND next_due_at <= ?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_map(params![now_ts], row_to_rule)?
            .collect::<rusqlite::Result<_>>()?
    };

    let mut out = Vec::new();
    for rule in due {
        let Some((keyholder_id, submissive_id)) = links::parties(conn, &rule.link_id)? else {
            continue;
        };

        if !rule.allow_overlap {
            let has_open: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM assignments
                    WHERE spawned_by_recurring_task_rule_id = ?1
                      AND status IN ('assigned','acknowledged','proof_submitted'))",
                params![rule.id],
                |row| row.get(0),
            )?;
            if has_open {
                continue;
            }
        }

        let Some(next_due_at) =
            next_occurrence(&rule.recurrence_kind, &rule.recurrence_value, now_ts + 1)
        else {
            continue;
        };

        let tx = conn.transaction()?;
        let spawned = assignments::create(
            &tx,
            &submissive_id,
            &rule.link_id,
            assignments::NewAssignment {
                kind: Some("task"),
                template_id: Some(&rule.template_id),
                require_active_template: true,
                spawned_by_recurring_task_rule_id: Some(&rule.id),
                assigned_via: "system",
                ..Default::default()
            },
        );
        match spawned {
            Ok(assignment) => {
                tx.execute(
                    "UPDATE recurring_task_rules SET next_due_at = ?1 WHERE id = ?2",
                    params![next_due_at, rule.id],
                )?;
                tx.commit()?;
                out.push(SpawnedTask {
                    keyholder_id,
                    submissive_id,
                    link_id: rule.link_id,
                    assignment,
                });
            }
            Err(_) => {
                // Most likely the underlying template was deactivated
                // since this rule was created — skip this tick and
                // leave `next_due_at` unchanged so it keeps retrying
                // until either the template or the rule is fixed up,
                // rather than silently going quiet forever.
                drop(tx);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn seed_task_template(conn: &Connection, kh: &str) -> String {
        templates::create(
            conn,
            kh,
            templates::NewTemplate {
                kind: "task",
                title: "Morning photo",
                description: None,
                severity: None,
                effect_kind: None,
                completion_type: Some("acknowledge_only"),
                proof_media_types: None,
                default_deadline_seconds: Some(3600),
                time_extension_seconds: None,
                time_reduction_seconds: None,
                on_success_template_id: None,
                on_failure_template_id: None,
                points_delta: None,
                points_cost: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn next_occurrence_interval_hours_adds_the_interval() {
        assert_eq!(
            next_occurrence("interval_hours", r#"{"hours":6}"#, 1000),
            Some(1000 + 6 * 3600)
        );
        assert_eq!(
            next_occurrence("interval_hours", r#"{"hours":0}"#, 1000),
            None
        );
    }

    #[test]
    fn next_occurrence_daily_rolls_to_tomorrow_if_todays_time_passed() {
        // 1970-01-01T00:00:10Z: 10 seconds past midnight.
        let from = 10;
        // Target 00:00:05 already passed today -> tomorrow.
        assert_eq!(
            next_occurrence("daily", r#"{"time":"00:00"}"#, from + 5),
            Some(86_400)
        );
        // Target still ahead today.
        assert_eq!(
            next_occurrence("daily", r#"{"time":"00:05"}"#, from),
            Some(300)
        );
    }

    #[test]
    fn next_occurrence_weekly_days_finds_the_next_matching_weekday() {
        // 1970-01-01 was a Thursday.
        let thursday_midnight = 0i64;
        // Requesting Monday only should land 4 days later.
        let next = next_occurrence(
            "weekly_days",
            r#"{"days":["mon"],"time":"00:00"}"#,
            thursday_midnight,
        )
        .unwrap();
        assert_eq!(next, 4 * 86_400);

        // Requesting Thursday itself, right at midnight, matches today.
        let next_today = next_occurrence(
            "weekly_days",
            r#"{"days":["thu"],"time":"00:00"}"#,
            thursday_midnight,
        )
        .unwrap();
        assert_eq!(next_today, 0);
    }

    #[test]
    fn create_rejects_a_non_task_template_and_someone_elses_template() {
        let (_dir, pool, kh, _sub, link) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        let reward_template_id = templates::create(
            &conn,
            &kh,
            templates::NewTemplate {
                kind: "reward",
                title: "Nice job",
                description: None,
                severity: None,
                effect_kind: Some("grant"),
                completion_type: None,
                proof_media_types: None,
                default_deadline_seconds: None,
                time_extension_seconds: None,
                time_reduction_seconds: None,
                on_success_template_id: None,
                on_failure_template_id: None,
                points_delta: None,
                points_cost: None,
            },
        )
        .unwrap();

        assert!(matches!(
            create(
                &conn,
                NewRule {
                    link_id: &link,
                    keyholder_id: &kh,
                    template_id: &reward_template_id,
                    recurrence_kind: "daily",
                    recurrence_value: r#"{"time":"09:00"}"#,
                    allow_overlap: false,
                },
            ),
            Err(RuleError::NotATaskTemplate)
        ));

        let other_kh = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH2', 0)",
            params![other_kh],
        )
        .unwrap();
        let task_template_id = seed_task_template(&conn, &kh);
        assert!(matches!(
            create(
                &conn,
                NewRule {
                    link_id: &link,
                    keyholder_id: &other_kh,
                    template_id: &task_template_id,
                    recurrence_kind: "daily",
                    recurrence_value: r#"{"time":"09:00"}"#,
                    allow_overlap: false,
                },
            ),
            Err(RuleError::TemplateNotFound)
        ));
    }

    #[test]
    fn sweep_spawns_a_due_rule_and_advances_next_due_at() {
        let (_dir, pool, kh, _sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = seed_task_template(&conn, &kh);
        let rule_id = create(
            &conn,
            NewRule {
                link_id: &link,
                keyholder_id: &kh,
                template_id: &template_id,
                recurrence_kind: "interval_hours",
                recurrence_value: r#"{"hours":6}"#,
                allow_overlap: false,
            },
        )
        .unwrap();
        // Back-date so it's immediately due.
        conn.execute(
            "UPDATE recurring_task_rules SET next_due_at = ?1 WHERE id = ?2",
            params![crate::auth::session::now() - 10, rule_id],
        )
        .unwrap();

        let spawned = run_recurring_task_sweep_tick(&mut conn).unwrap();
        assert_eq!(spawned.len(), 1);
        assert_eq!(
            spawned[0]
                .assignment
                .spawned_by_recurring_task_rule_id
                .as_deref(),
            Some(rule_id.as_str())
        );
        assert_eq!(spawned[0].assignment.assigned_via, "system");

        let rule = get(&conn, &rule_id).unwrap().unwrap();
        assert!(rule.next_due_at > crate::auth::session::now());

        // A second tick right away finds nothing due.
        assert!(run_recurring_task_sweep_tick(&mut conn).unwrap().is_empty());
    }

    #[test]
    fn sweep_skips_and_leaves_next_due_at_unchanged_while_the_previous_spawn_is_still_open() {
        let (_dir, pool, kh, _sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = seed_task_template(&conn, &kh);
        let rule_id = create(
            &conn,
            NewRule {
                link_id: &link,
                keyholder_id: &kh,
                template_id: &template_id,
                recurrence_kind: "interval_hours",
                recurrence_value: r#"{"hours":6}"#,
                allow_overlap: false,
            },
        )
        .unwrap();
        conn.execute(
            "UPDATE recurring_task_rules SET next_due_at = ?1 WHERE id = ?2",
            params![crate::auth::session::now() - 10, rule_id],
        )
        .unwrap();

        let first = run_recurring_task_sweep_tick(&mut conn).unwrap();
        assert_eq!(first.len(), 1);
        let next_due_after_first = get(&conn, &rule_id).unwrap().unwrap().next_due_at;

        // Force it due again while the first spawn is still open.
        conn.execute(
            "UPDATE recurring_task_rules SET next_due_at = ?1 WHERE id = ?2",
            params![crate::auth::session::now() - 10, rule_id],
        )
        .unwrap();
        let second = run_recurring_task_sweep_tick(&mut conn).unwrap();
        assert!(second.is_empty());
        // next_due_at is exactly what we just forced it to, i.e.
        // untouched by the skipped tick.
        assert_eq!(
            get(&conn, &rule_id).unwrap().unwrap().next_due_at,
            crate::auth::session::now() - 10
        );
        assert_ne!(next_due_after_first, crate::auth::session::now() - 10);
    }

    #[test]
    fn sweep_respects_allow_overlap() {
        let (_dir, pool, kh, _sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = seed_task_template(&conn, &kh);
        let rule_id = create(
            &conn,
            NewRule {
                link_id: &link,
                keyholder_id: &kh,
                template_id: &template_id,
                recurrence_kind: "interval_hours",
                recurrence_value: r#"{"hours":1}"#,
                allow_overlap: true,
            },
        )
        .unwrap();
        conn.execute(
            "UPDATE recurring_task_rules SET next_due_at = ?1 WHERE id = ?2",
            params![crate::auth::session::now() - 10, rule_id],
        )
        .unwrap();
        assert_eq!(run_recurring_task_sweep_tick(&mut conn).unwrap().len(), 1);

        conn.execute(
            "UPDATE recurring_task_rules SET next_due_at = ?1 WHERE id = ?2",
            params![crate::auth::session::now() - 10, rule_id],
        )
        .unwrap();
        // Still spawns a second time even though the first is open,
        // because allow_overlap is set.
        assert_eq!(run_recurring_task_sweep_tick(&mut conn).unwrap().len(), 1);
    }

    #[test]
    fn update_recomputes_next_due_at_only_when_the_schedule_changes() {
        let (_dir, pool, kh, _sub, link) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        let template_id = seed_task_template(&conn, &kh);
        let rule_id = create(
            &conn,
            NewRule {
                link_id: &link,
                keyholder_id: &kh,
                template_id: &template_id,
                recurrence_kind: "interval_hours",
                recurrence_value: r#"{"hours":6}"#,
                allow_overlap: false,
            },
        )
        .unwrap();
        let original_next_due_at = get(&conn, &rule_id).unwrap().unwrap().next_due_at;

        // Toggling `active` alone doesn't touch the schedule.
        assert!(
            update(
                &conn,
                &rule_id,
                &link,
                RuleEdit {
                    active: Some(false),
                    ..Default::default()
                },
            )
            .unwrap()
        );
        assert_eq!(
            get(&conn, &rule_id).unwrap().unwrap().next_due_at,
            original_next_due_at
        );

        // Changing the recurrence value recomputes it from now.
        update(
            &conn,
            &rule_id,
            &link,
            RuleEdit {
                recurrence_value: Some(r#"{"hours":1}"#),
                ..Default::default()
            },
        )
        .unwrap();
        let updated = get(&conn, &rule_id).unwrap().unwrap();
        assert!(updated.next_due_at < original_next_due_at);

        // Scoped to the owning link.
        assert!(!update(&conn, &rule_id, "someone-elses-link", RuleEdit::default()).unwrap());
    }
}
