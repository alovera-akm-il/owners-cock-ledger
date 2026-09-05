//! Play sessions (01-data-model.md §15, 14-play-sessions.md) —
//! Keyholder-authored, submissive-agnostic templates
//! (`suggested_toy_categories` instead of real toy references, since
//! toys are per-submissive but a template must be reusable across
//! every submissive a Keyholder oversees) plus actual instances that
//! cover both live and retrospective logging with one state machine:
//! `scheduled -> in_progress -> pending_judgement -> completed`, with
//! a `cancelled` branch from `scheduled`/`in_progress`. Judgement
//! reuses the existing `assignments`/`reward_punishment_templates`
//! machinery rather than a parallel consequence system. The real-time
//! SSE fan-out for a live session's check-in stream (13-checkins.md
//! §5) is Phase 7 and out of scope here.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::domain::rewards_punishments::assignments;
use crate::domain::toys;

pub struct Template {
    pub id: String,
    pub keyholder_id: String,
    pub title: String,
    pub setup_notes: Option<String>,
    pub suggested_toy_categories: Option<String>,
    pub planned_duration_seconds: Option<i64>,
    pub checkin_template_id: Option<String>,
    pub checkin_interval_seconds: Option<i64>,
    pub active: bool,
    pub created_at: i64,
}

const TEMPLATE_COLUMNS: &str = "id, keyholder_id, title, setup_notes, suggested_toy_categories, \
     planned_duration_seconds, checkin_template_id, checkin_interval_seconds, active, created_at";

fn row_to_template(row: &rusqlite::Row) -> rusqlite::Result<Template> {
    Ok(Template {
        id: row.get(0)?,
        keyholder_id: row.get(1)?,
        title: row.get(2)?,
        setup_notes: row.get(3)?,
        suggested_toy_categories: row.get(4)?,
        planned_duration_seconds: row.get(5)?,
        checkin_template_id: row.get(6)?,
        checkin_interval_seconds: row.get(7)?,
        active: row.get(8)?,
        created_at: row.get(9)?,
    })
}

pub fn get_template(conn: &Connection, id: &str) -> rusqlite::Result<Option<Template>> {
    conn.query_row(
        &format!("SELECT {TEMPLATE_COLUMNS} FROM play_session_templates WHERE id = ?1"),
        params![id],
        row_to_template,
    )
    .optional()
}

pub fn list_templates_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<Template>> {
    let sql = format!(
        "SELECT {TEMPLATE_COLUMNS} FROM play_session_templates WHERE keyholder_id = ?1 ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![keyholder_id], row_to_template)?
        .collect()
}

pub struct NewTemplate<'a> {
    pub keyholder_id: &'a str,
    pub title: &'a str,
    pub setup_notes: Option<&'a str>,
    pub suggested_toy_categories: Option<&'a str>,
    pub planned_duration_seconds: Option<i64>,
    pub checkin_template_id: Option<&'a str>,
    pub checkin_interval_seconds: Option<i64>,
}

/// `POST /keyholder/play-session-templates` (03-api-design.md §10).
pub fn create_template(conn: &Connection, new: NewTemplate) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO play_session_templates
            (id, keyholder_id, title, setup_notes, suggested_toy_categories,
             planned_duration_seconds, checkin_template_id, checkin_interval_seconds, active, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,?9)",
        params![
            id,
            new.keyholder_id,
            new.title,
            new.setup_notes,
            new.suggested_toy_categories,
            new.planned_duration_seconds,
            new.checkin_template_id,
            new.checkin_interval_seconds,
            crate::auth::session::now(),
        ],
    )?;
    Ok(id)
}

#[derive(Default)]
pub struct TemplateEdit<'a> {
    pub title: Option<&'a str>,
    pub setup_notes: Option<Option<&'a str>>,
    pub suggested_toy_categories: Option<Option<&'a str>>,
    pub planned_duration_seconds: Option<Option<i64>>,
    pub checkin_template_id: Option<Option<&'a str>>,
    pub checkin_interval_seconds: Option<Option<i64>>,
    pub active: Option<bool>,
}

/// `PATCH /keyholder/play-session-templates/{id}` — never rewrites a
/// session already created from this template (copy-at-creation, same
/// as every other template in this schema). Returns `false` if no
/// such template belongs to this Keyholder.
pub fn update_template(
    conn: &Connection,
    id: &str,
    keyholder_id: &str,
    edit: TemplateEdit,
) -> rusqlite::Result<bool> {
    let Some(current) = get_template(conn, id)? else {
        return Ok(false);
    };
    if current.keyholder_id != keyholder_id {
        return Ok(false);
    }
    let title = edit.title.unwrap_or(&current.title);
    let setup_notes = edit.setup_notes.unwrap_or(current.setup_notes.as_deref());
    let suggested_toy_categories = edit
        .suggested_toy_categories
        .unwrap_or(current.suggested_toy_categories.as_deref());
    let planned_duration_seconds = edit
        .planned_duration_seconds
        .unwrap_or(current.planned_duration_seconds);
    let checkin_template_id = edit
        .checkin_template_id
        .unwrap_or(current.checkin_template_id.as_deref());
    let checkin_interval_seconds = edit
        .checkin_interval_seconds
        .unwrap_or(current.checkin_interval_seconds);
    let active = edit.active.unwrap_or(current.active);

    conn.execute(
        "UPDATE play_session_templates SET title = ?1, setup_notes = ?2, suggested_toy_categories = ?3,
            planned_duration_seconds = ?4, checkin_template_id = ?5, checkin_interval_seconds = ?6, active = ?7
         WHERE id = ?8",
        params![
            title,
            setup_notes,
            suggested_toy_categories,
            planned_duration_seconds,
            checkin_template_id,
            checkin_interval_seconds,
            active,
            id,
        ],
    )?;
    Ok(true)
}

pub struct PlaySession {
    pub id: String,
    pub link_id: String,
    pub template_id: Option<String>,
    pub title: String,
    pub setup_notes: Option<String>,
    pub status: String,
    pub planned_duration_seconds: Option<i64>,
    pub checkin_template_id: Option<String>,
    pub checkin_interval_seconds: Option<i64>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub safety_check_ok: Option<bool>,
    pub judgement_notes: Option<String>,
    pub reward_assignment_id: Option<String>,
    pub punishment_assignment_id: Option<String>,
    pub assigned_by_user_id: String,
    pub assigned_at: i64,
}

const SESSION_COLUMNS: &str = "id, link_id, template_id, title, setup_notes, status, \
     planned_duration_seconds, checkin_template_id, checkin_interval_seconds, started_at, ended_at, \
     safety_check_ok, judgement_notes, reward_assignment_id, punishment_assignment_id, \
     assigned_by_user_id, assigned_at";

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<PlaySession> {
    Ok(PlaySession {
        id: row.get(0)?,
        link_id: row.get(1)?,
        template_id: row.get(2)?,
        title: row.get(3)?,
        setup_notes: row.get(4)?,
        status: row.get(5)?,
        planned_duration_seconds: row.get(6)?,
        checkin_template_id: row.get(7)?,
        checkin_interval_seconds: row.get(8)?,
        started_at: row.get(9)?,
        ended_at: row.get(10)?,
        safety_check_ok: row.get(11)?,
        judgement_notes: row.get(12)?,
        reward_assignment_id: row.get(13)?,
        punishment_assignment_id: row.get(14)?,
        assigned_by_user_id: row.get(15)?,
        assigned_at: row.get(16)?,
    })
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<PlaySession>> {
    conn.query_row(
        &format!("SELECT {SESSION_COLUMNS} FROM play_sessions WHERE id = ?1"),
        params![id],
        row_to_session,
    )
    .optional()
}

/// `GET .../play-sessions` — filterable by `status`.
pub fn list_for_link(
    conn: &Connection,
    link_id: &str,
    status: Option<&str>,
) -> rusqlite::Result<Vec<PlaySession>> {
    let mut sql = format!("SELECT {SESSION_COLUMNS} FROM play_sessions WHERE link_id = ?1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(link_id.to_string())];
    if let Some(s) = status {
        sql.push_str(" AND status = ?");
        params.push(Box::new(s.to_string()));
    }
    sql.push_str(" ORDER BY assigned_at DESC");
    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    stmt.query_map(param_refs.as_slice(), row_to_session)?
        .collect()
}

pub fn toy_ids_for_session(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT toy_id FROM play_session_toys WHERE session_id = ?1")?;
    stmt.query_map(params![session_id], |row| row.get(0))?
        .collect()
}

pub struct ScheduleSlot {
    #[allow(dead_code)]
    pub id: String,
    #[allow(dead_code)]
    pub play_session_id: String,
    pub sequence_number: i64,
    pub planned_offset_seconds: i64,
    pub checkin_template_id: String,
    pub fulfilled_checkin_id: Option<String>,
}

const SCHEDULE_COLUMNS: &str = "id, play_session_id, sequence_number, planned_offset_seconds, checkin_template_id, fulfilled_checkin_id";

fn row_to_slot(row: &rusqlite::Row) -> rusqlite::Result<ScheduleSlot> {
    Ok(ScheduleSlot {
        id: row.get(0)?,
        play_session_id: row.get(1)?,
        sequence_number: row.get(2)?,
        planned_offset_seconds: row.get(3)?,
        checkin_template_id: row.get(4)?,
        fulfilled_checkin_id: row.get(5)?,
    })
}

pub fn schedule_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<ScheduleSlot>> {
    let sql = format!(
        "SELECT {SCHEDULE_COLUMNS} FROM play_session_checkin_schedule WHERE play_session_id = ?1 ORDER BY sequence_number ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![session_id], row_to_slot)?.collect()
}

/// Called from the check-ins API layer when a check-in comes in with
/// `related_play_session_id` set: fills the earliest still-open
/// schedule slot with this check-in's id, so the UI can show "N of M
/// done." A simple earliest-open-slot match, since the schedule
/// doesn't track wall-clock due times yet (that sweep is deferred with
/// `play_session.checkin_due`, same as the SSE stream). Returns
/// `false` if there was no open slot to fill (e.g. an ad-hoc check-in
/// beyond the scheduled count).
pub fn fulfill_next_schedule_slot(
    conn: &Connection,
    play_session_id: &str,
    checkin_id: &str,
) -> rusqlite::Result<bool> {
    let slot_id: Option<String> = conn
        .query_row(
            "SELECT id FROM play_session_checkin_schedule
             WHERE play_session_id = ?1 AND fulfilled_checkin_id IS NULL
             ORDER BY sequence_number ASC LIMIT 1",
            params![play_session_id],
            |row| row.get(0),
        )
        .optional()?;
    match slot_id {
        Some(slot_id) => {
            conn.execute(
                "UPDATE play_session_checkin_schedule SET fulfilled_checkin_id = ?1 WHERE id = ?2",
                params![checkin_id, slot_id],
            )?;
            Ok(true)
        }
        None => Ok(false),
    }
}

#[derive(Debug, Error)]
pub enum CreateError {
    #[error("template not found, or not this keyholder's own")]
    TemplateNotFound,
    #[error("title is required")]
    MissingTitle,
    #[error("a toy does not exist, or does not belong to this submissive")]
    InvalidToy,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub struct NewSession<'a> {
    pub link_id: &'a str,
    pub submissive_id: &'a str,
    pub template_id: Option<&'a str>,
    pub title: Option<&'a str>,
    pub setup_notes: Option<&'a str>,
    pub toy_ids: &'a [String],
    pub planned_duration_seconds: Option<i64>,
    pub checkin_template_id: Option<&'a str>,
    pub checkin_interval_seconds: Option<i64>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub assigned_by_user_id: &'a str,
}

/// `POST /keyholder/submissives/{id}/play-sessions` (03-api-design.md
/// §10, 14-play-sessions.md §3/§4). Omitting `started_at` creates a
/// `scheduled` session for a live start later; supplying both
/// `started_at` and `ended_at` logs a retrospective session, landing
/// directly in `pending_judgement`. Supplying `started_at` alone
/// (undocumented edge case, not covered by §3's two named paths) is
/// treated as an already-live session (`in_progress`) rather than
/// rejected. When a check-in template + interval + duration are all
/// present, generates `floor(duration / interval)` schedule slots at
/// `interval, 2*interval, ...` offsets (14-play-sessions.md §4's
/// 60-min/20-min/3-slots example).
pub fn create(conn: &mut Connection, new: NewSession) -> Result<PlaySession, CreateError> {
    let tx = conn.transaction()?;

    let template = match new.template_id {
        Some(tid) => Some(get_template(&tx, tid)?.ok_or(CreateError::TemplateNotFound)?),
        None => None,
    };
    let title = new
        .title
        .map(str::to_string)
        .or_else(|| template.as_ref().map(|t| t.title.clone()))
        .ok_or(CreateError::MissingTitle)?;
    let setup_notes = new
        .setup_notes
        .map(str::to_string)
        .or_else(|| template.as_ref().and_then(|t| t.setup_notes.clone()));
    let planned_duration_seconds = new
        .planned_duration_seconds
        .or_else(|| template.as_ref().and_then(|t| t.planned_duration_seconds));
    let checkin_template_id = new.checkin_template_id.map(str::to_string).or_else(|| {
        template
            .as_ref()
            .and_then(|t| t.checkin_template_id.clone())
    });
    let checkin_interval_seconds = new
        .checkin_interval_seconds
        .or_else(|| template.as_ref().and_then(|t| t.checkin_interval_seconds));

    for toy_id in new.toy_ids {
        let toy = toys::get(&tx, toy_id)?.ok_or(CreateError::InvalidToy)?;
        if toy.submissive_id != new.submissive_id {
            return Err(CreateError::InvalidToy);
        }
    }

    let status = if new.started_at.is_some() && new.ended_at.is_some() {
        "pending_judgement"
    } else if new.started_at.is_some() {
        "in_progress"
    } else {
        "scheduled"
    };

    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::auth::session::now();
    tx.execute(
        "INSERT INTO play_sessions
            (id, link_id, template_id, title, setup_notes, status, planned_duration_seconds,
             checkin_template_id, checkin_interval_seconds, started_at, ended_at,
             assigned_by_user_id, assigned_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            id,
            new.link_id,
            new.template_id,
            title,
            setup_notes,
            status,
            planned_duration_seconds,
            checkin_template_id,
            checkin_interval_seconds,
            new.started_at,
            new.ended_at,
            new.assigned_by_user_id,
            now,
        ],
    )?;

    for toy_id in new.toy_ids {
        tx.execute(
            "INSERT INTO play_session_toys (session_id, toy_id) VALUES (?1, ?2)",
            params![id, toy_id],
        )?;
    }

    if let (Some(cti), Some(interval), Some(duration)) = (
        &checkin_template_id,
        checkin_interval_seconds,
        planned_duration_seconds,
    ) && interval > 0
    {
        let slots = duration / interval;
        for seq in 1..=slots {
            tx.execute(
                "INSERT INTO play_session_checkin_schedule
                    (id, play_session_id, sequence_number, planned_offset_seconds, checkin_template_id)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    id,
                    seq,
                    seq * interval,
                    cti,
                ],
            )?;
        }
    }

    tx.commit()?;
    Ok(get(conn, &id)?.expect("just inserted"))
}

#[derive(Debug, Error)]
pub enum StartError {
    #[error("session not found")]
    NotFound,
    #[error("not in a startable state")]
    Conflict,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST .../play-sessions/{id}/start` — either role, for their own
/// link. `409` if not `scheduled`.
pub fn start(conn: &Connection, id: &str) -> Result<PlaySession, StartError> {
    let session = get(conn, id)?.ok_or(StartError::NotFound)?;
    if session.status != "scheduled" {
        return Err(StartError::Conflict);
    }
    conn.execute(
        "UPDATE play_sessions SET status = 'in_progress', started_at = ?1 WHERE id = ?2",
        params![crate::auth::session::now(), id],
    )?;
    Ok(get(conn, id)?.expect("just updated"))
}

#[derive(Debug, Error)]
pub enum EndError {
    #[error("session not found")]
    NotFound,
    #[error("not in an endable state")]
    Conflict,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST .../play-sessions/{id}/end` — either role. `409` if not
/// `in_progress`.
pub fn end(
    conn: &Connection,
    id: &str,
    safety_check_ok: Option<bool>,
) -> Result<PlaySession, EndError> {
    let session = get(conn, id)?.ok_or(EndError::NotFound)?;
    if session.status != "in_progress" {
        return Err(EndError::Conflict);
    }
    conn.execute(
        "UPDATE play_sessions SET status = 'pending_judgement', ended_at = ?1, safety_check_ok = ?2
         WHERE id = ?3",
        params![crate::auth::session::now(), safety_check_ok, id],
    )?;
    Ok(get(conn, id)?.expect("just updated"))
}

#[derive(Debug, Error)]
pub enum JudgementError {
    #[error("session not found")]
    NotFound,
    #[error("session is already completed")]
    AlreadyCompleted,
    #[error(transparent)]
    Assignment(#[from] assignments::CreateError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub struct JudgementConsequence<'a> {
    pub template_id: Option<&'a str>,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub effect_kind: Option<&'a str>,
    pub time_extension_seconds: Option<i64>,
    pub time_reduction_seconds: Option<i64>,
    pub points_delta: Option<i64>,
}

#[derive(Default)]
pub struct Judgement<'a> {
    pub judgement_notes: Option<&'a str>,
    pub reward: Option<JudgementConsequence<'a>>,
    pub punishment: Option<JudgementConsequence<'a>>,
}

/// `PATCH /keyholder/play-sessions/{id}/judgement` — reuses
/// `assignments::create` for any reward/punishment, each with
/// `triggered_by_play_session_id` pointing back here
/// (14-play-sessions.md §5); records the resulting id(s) back onto
/// the session so the link is navigable from either side. Callable
/// multiple times before `complete`; `409` once `completed`.
pub fn judge(
    conn: &mut Connection,
    id: &str,
    submissive_id: &str,
    keyholder_id: &str,
    j: Judgement,
) -> Result<PlaySession, JudgementError> {
    let tx = conn.transaction()?;

    let session = get(&tx, id)?.ok_or(JudgementError::NotFound)?;
    if session.status == "completed" {
        return Err(JudgementError::AlreadyCompleted);
    }

    let mut reward_assignment_id = session.reward_assignment_id.clone();
    let mut punishment_assignment_id = session.punishment_assignment_id.clone();

    if let Some(r) = j.reward {
        let a = assignments::create(
            &tx,
            submissive_id,
            &session.link_id,
            assignments::NewAssignment {
                kind: Some("reward"),
                template_id: r.template_id,
                require_active_template: false,
                title: r.title,
                description: r.description,
                effect_kind: r.effect_kind,
                time_extension_seconds: r.time_extension_seconds,
                time_reduction_seconds: r.time_reduction_seconds,
                points_delta: r.points_delta,
                triggered_by_play_session_id: Some(id),
                assigned_by_user_id: Some(keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )?;
        reward_assignment_id = Some(a.id);
    }
    if let Some(p) = j.punishment {
        let a = assignments::create(
            &tx,
            submissive_id,
            &session.link_id,
            assignments::NewAssignment {
                kind: Some("punishment"),
                template_id: p.template_id,
                require_active_template: false,
                title: p.title,
                description: p.description,
                effect_kind: p.effect_kind,
                time_extension_seconds: p.time_extension_seconds,
                time_reduction_seconds: p.time_reduction_seconds,
                points_delta: p.points_delta,
                triggered_by_play_session_id: Some(id),
                assigned_by_user_id: Some(keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )?;
        punishment_assignment_id = Some(a.id);
    }

    let judgement_notes = j.judgement_notes.or(session.judgement_notes.as_deref());
    tx.execute(
        "UPDATE play_sessions SET judgement_notes = ?1, reward_assignment_id = ?2, punishment_assignment_id = ?3
         WHERE id = ?4",
        params![judgement_notes, reward_assignment_id, punishment_assignment_id, id],
    )?;

    tx.commit()?;
    Ok(get(conn, id)?.expect("just updated"))
}

#[derive(Debug, Error)]
pub enum CompleteError {
    #[error("session not found")]
    NotFound,
    #[error("not in a completable state")]
    Conflict,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PATCH /keyholder/play-sessions/{id}/complete` — judgement is
/// optional; `409` unless `pending_judgement`.
pub fn complete(conn: &Connection, id: &str) -> Result<PlaySession, CompleteError> {
    let session = get(conn, id)?.ok_or(CompleteError::NotFound)?;
    if session.status != "pending_judgement" {
        return Err(CompleteError::Conflict);
    }
    conn.execute(
        "UPDATE play_sessions SET status = 'completed' WHERE id = ?1",
        params![id],
    )?;
    Ok(get(conn, id)?.expect("just updated"))
}

#[derive(Debug, Error)]
pub enum CancelError {
    #[error("session not found")]
    NotFound,
    #[error("not in a cancellable state")]
    Conflict,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PATCH /keyholder/play-sessions/{id}/cancel` — from `scheduled` or
/// `in_progress` only; no judgement applies.
pub fn cancel(conn: &Connection, id: &str) -> Result<PlaySession, CancelError> {
    let session = get(conn, id)?.ok_or(CancelError::NotFound)?;
    if session.status != "scheduled" && session.status != "in_progress" {
        return Err(CancelError::Conflict);
    }
    conn.execute(
        "UPDATE play_sessions SET status = 'cancelled' WHERE id = ?1",
        params![id],
    )?;
    Ok(get(conn, id)?.expect("just updated"))
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
        let link_id = crate::domain::links::create(&conn, &keyholder_id, &submissive_id).unwrap();
        (dir, pool, keyholder_id, submissive_id, link_id)
    }

    fn seed_toy(conn: &Connection, submissive_id: &str) -> String {
        toys::create(
            conn,
            toys::NewToy {
                submissive_id,
                added_by_user_id: submissive_id,
                name: "steel cage",
                category: None,
                material: None,
                brand: None,
                size_notes: None,
                color: None,
                compatible_device_id: None,
                storage_location: None,
                care_instructions: None,
                usage_notes: None,
                tags: None,
                photo_attachment_path: None,
                acquired_at: None,
            },
        )
        .unwrap()
    }

    fn seed_checkin_template(conn: &Connection, kh: &str) -> String {
        crate::domain::checkins::create_template(conn, kh, "Mid-session check", None, false, &[])
            .unwrap()
    }

    #[test]
    fn create_from_scratch_defaults_to_scheduled_and_generates_no_schedule_without_a_duration() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let toy_id = seed_toy(&conn, &sub);

        let session = create(
            &mut conn,
            NewSession {
                link_id: &link,
                submissive_id: &sub,
                template_id: None,
                title: Some("Evening session"),
                setup_notes: None,
                toy_ids: std::slice::from_ref(&toy_id),
                planned_duration_seconds: None,
                checkin_template_id: None,
                checkin_interval_seconds: None,
                started_at: None,
                ended_at: None,
                assigned_by_user_id: &kh,
            },
        )
        .unwrap();

        assert_eq!(session.status, "scheduled");
        assert_eq!(
            toy_ids_for_session(&conn, &session.id).unwrap(),
            vec![toy_id]
        );
        assert!(schedule_for_session(&conn, &session.id).unwrap().is_empty());
    }

    #[test]
    fn create_rejects_a_toy_belonging_to_another_submissive() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let other_sub = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Other', 0)",
            params![other_sub],
        )
        .unwrap();
        let toy_id = seed_toy(&conn, &other_sub);

        let result = create(
            &mut conn,
            NewSession {
                link_id: &link,
                submissive_id: &sub,
                template_id: None,
                title: Some("Evening session"),
                setup_notes: None,
                toy_ids: &[toy_id],
                planned_duration_seconds: None,
                checkin_template_id: None,
                checkin_interval_seconds: None,
                started_at: None,
                ended_at: None,
                assigned_by_user_id: &kh,
            },
        );
        assert!(matches!(result, Err(CreateError::InvalidToy)));
    }

    #[test]
    fn create_generates_the_documented_three_slot_schedule() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let checkin_template_id = seed_checkin_template(&conn, &kh);

        let session = create(
            &mut conn,
            NewSession {
                link_id: &link,
                submissive_id: &sub,
                template_id: None,
                title: Some("Hour-long session"),
                setup_notes: None,
                toy_ids: &[],
                planned_duration_seconds: Some(3600),
                checkin_template_id: Some(&checkin_template_id),
                checkin_interval_seconds: Some(1200),
                started_at: None,
                ended_at: None,
                assigned_by_user_id: &kh,
            },
        )
        .unwrap();

        let schedule = schedule_for_session(&conn, &session.id).unwrap();
        assert_eq!(schedule.len(), 3);
        assert_eq!(schedule[0].planned_offset_seconds, 1200);
        assert_eq!(schedule[2].planned_offset_seconds, 3600);
        assert!(schedule.iter().all(|s| s.fulfilled_checkin_id.is_none()));
    }

    #[test]
    fn supplying_both_started_and_ended_at_lands_directly_in_pending_judgement() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let session = create(
            &mut conn,
            NewSession {
                link_id: &link,
                submissive_id: &sub,
                template_id: None,
                title: Some("Logged after the fact"),
                setup_notes: None,
                toy_ids: &[],
                planned_duration_seconds: None,
                checkin_template_id: None,
                checkin_interval_seconds: None,
                started_at: Some(1000),
                ended_at: Some(2000),
                assigned_by_user_id: &kh,
            },
        )
        .unwrap();
        assert_eq!(session.status, "pending_judgement");
    }

    #[test]
    fn start_end_judge_complete_lifecycle() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let session = create(
            &mut conn,
            NewSession {
                link_id: &link,
                submissive_id: &sub,
                template_id: None,
                title: Some("Live session"),
                setup_notes: None,
                toy_ids: &[],
                planned_duration_seconds: None,
                checkin_template_id: None,
                checkin_interval_seconds: None,
                started_at: None,
                ended_at: None,
                assigned_by_user_id: &kh,
            },
        )
        .unwrap();

        // Can't end before starting.
        assert!(matches!(
            end(&conn, &session.id, None),
            Err(EndError::Conflict)
        ));

        let started = start(&conn, &session.id).unwrap();
        assert_eq!(started.status, "in_progress");
        assert!(started.started_at.is_some());

        // Can't start twice.
        assert!(matches!(
            start(&conn, &session.id),
            Err(StartError::Conflict)
        ));

        let ended = end(&conn, &session.id, Some(true)).unwrap();
        assert_eq!(ended.status, "pending_judgement");
        assert_eq!(ended.safety_check_ok, Some(true));

        let judged = judge(
            &mut conn,
            &session.id,
            &sub,
            &kh,
            Judgement {
                judgement_notes: Some("Went well"),
                reward: Some(JudgementConsequence {
                    template_id: None,
                    title: Some("Extra praise"),
                    description: Some("Well done"),
                    effect_kind: Some("time_reduction"),
                    time_extension_seconds: None,
                    time_reduction_seconds: Some(3600),
                    points_delta: None,
                }),
                punishment: None,
            },
        )
        .unwrap();
        assert_eq!(judged.judgement_notes.as_deref(), Some("Went well"));
        assert!(judged.reward_assignment_id.is_some());
        assert!(judged.punishment_assignment_id.is_none());

        let completed = complete(&conn, &session.id).unwrap();
        assert_eq!(completed.status, "completed");

        // Judgement is rejected once completed.
        assert!(matches!(
            judge(&mut conn, &session.id, &sub, &kh, Judgement::default()),
            Err(JudgementError::AlreadyCompleted)
        ));
        // Completing twice is rejected.
        assert!(matches!(
            complete(&conn, &session.id),
            Err(CompleteError::Conflict)
        ));
    }

    #[test]
    fn cancel_only_works_from_scheduled_or_in_progress() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let session = create(
            &mut conn,
            NewSession {
                link_id: &link,
                submissive_id: &sub,
                template_id: None,
                title: Some("To be cancelled"),
                setup_notes: None,
                toy_ids: &[],
                planned_duration_seconds: None,
                checkin_template_id: None,
                checkin_interval_seconds: None,
                started_at: None,
                ended_at: None,
                assigned_by_user_id: &kh,
            },
        )
        .unwrap();

        let cancelled = cancel(&conn, &session.id).unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert!(matches!(
            cancel(&conn, &session.id),
            Err(CancelError::Conflict)
        ));
    }

    #[test]
    fn fulfill_next_schedule_slot_fills_earliest_open_slot_in_order() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let checkin_template_id = seed_checkin_template(&conn, &kh);

        let session = create(
            &mut conn,
            NewSession {
                link_id: &link,
                submissive_id: &sub,
                template_id: None,
                title: Some("Scheduled check-ins"),
                setup_notes: None,
                toy_ids: &[],
                planned_duration_seconds: Some(2400),
                checkin_template_id: Some(&checkin_template_id),
                checkin_interval_seconds: Some(1200),
                started_at: None,
                ended_at: None,
                assigned_by_user_id: &kh,
            },
        )
        .unwrap();

        let mk_checkin = |conn: &mut Connection| {
            crate::domain::checkins::create_checkin(
                conn,
                crate::domain::checkins::NewCheckin {
                    link_id: &link,
                    template_id: &checkin_template_id,
                    color: "green",
                    field_values: "{}",
                    related_confinement_session_id: None,
                    related_assignment_id: None,
                    related_play_session_id: Some(&session.id),
                    created_by_user_id: &sub,
                    has_photo: false,
                    has_audio: false,
                },
                &sub,
            )
            .unwrap()
            .0
        };
        let checkin_a = mk_checkin(&mut conn);
        let checkin_b = mk_checkin(&mut conn);
        let checkin_c = mk_checkin(&mut conn);

        assert!(fulfill_next_schedule_slot(&conn, &session.id, &checkin_a).unwrap());
        assert!(fulfill_next_schedule_slot(&conn, &session.id, &checkin_b).unwrap());
        // No more open slots.
        assert!(!fulfill_next_schedule_slot(&conn, &session.id, &checkin_c).unwrap());

        let schedule = schedule_for_session(&conn, &session.id).unwrap();
        assert_eq!(
            schedule[0].fulfilled_checkin_id.as_deref(),
            Some(checkin_a.as_str())
        );
        assert_eq!(
            schedule[1].fulfilled_checkin_id.as_deref(),
            Some(checkin_b.as_str())
        );
    }

    #[test]
    fn update_template_toggles_active_without_mutating_a_past_session() {
        let (_dir, pool, kh, sub, link) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let template_id = create_template(
            &conn,
            NewTemplate {
                keyholder_id: &kh,
                title: "Original title",
                setup_notes: None,
                suggested_toy_categories: None,
                planned_duration_seconds: None,
                checkin_template_id: None,
                checkin_interval_seconds: None,
            },
        )
        .unwrap();

        let session = create(
            &mut conn,
            NewSession {
                link_id: &link,
                submissive_id: &sub,
                template_id: Some(&template_id),
                title: None,
                setup_notes: None,
                toy_ids: &[],
                planned_duration_seconds: None,
                checkin_template_id: None,
                checkin_interval_seconds: None,
                started_at: None,
                ended_at: None,
                assigned_by_user_id: &kh,
            },
        )
        .unwrap();
        assert_eq!(session.title, "Original title");

        update_template(
            &conn,
            &template_id,
            &kh,
            TemplateEdit {
                title: Some("Renamed title"),
                active: Some(false),
                ..Default::default()
            },
        )
        .unwrap();

        let refetched = get(&conn, &session.id).unwrap().unwrap();
        assert_eq!(refetched.title, "Original title");
        assert!(!get_template(&conn, &template_id).unwrap().unwrap().active);
    }
}
