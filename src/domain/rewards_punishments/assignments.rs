//! `assignments` (01-data-model.md §6, 08-punishments-and-deadlines.md):
//! an actual instance of a reward, punishment, or task given to a
//! specific submissive — creation, the state machines, escalation, and
//! the deadline sweeper.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use super::templates;
use crate::auth::session::now;
use crate::domain::chastity::confinement::{self, ApplyEffect};
use crate::domain::{audit, points, proofs};

#[derive(Clone)]
pub struct Assignment {
    pub id: String,
    pub link_id: String,
    pub template_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub description: Option<String>,
    pub effect_kind: Option<String>,
    pub completion_type: Option<String>,
    pub proof_media_types: Option<String>,
    pub deadline_at: Option<i64>,
    pub time_extension_seconds: Option<i64>,
    pub time_reduction_seconds: Option<i64>,
    pub proof_submission_id: Option<String>,
    pub on_success_template_id: Option<String>,
    pub on_failure_template_id: Option<String>,
    pub escalated_from_assignment_id: Option<String>,
    pub triggered_by_submission_id: Option<String>,
    pub triggered_by_play_session_id: Option<String>,
    pub spawned_by_recurring_task_rule_id: Option<String>,
    pub points_delta: Option<i64>,
    pub assigned_at: i64,
    pub assigned_by_user_id: Option<String>,
    pub assigned_via: String,
    pub status: String,
    pub notes: Option<String>,
}

const COLUMNS: &str = "id, link_id, template_id, kind, title, description, effect_kind, \
     completion_type, proof_media_types, deadline_at, time_extension_seconds, \
     time_reduction_seconds, proof_submission_id, on_success_template_id, \
     on_failure_template_id, escalated_from_assignment_id, triggered_by_submission_id, \
     triggered_by_play_session_id, spawned_by_recurring_task_rule_id, points_delta, assigned_at, assigned_by_user_id, \
     assigned_via, status, notes";

fn row_to_assignment(row: &rusqlite::Row) -> rusqlite::Result<Assignment> {
    Ok(Assignment {
        id: row.get(0)?,
        link_id: row.get(1)?,
        template_id: row.get(2)?,
        kind: row.get(3)?,
        title: row.get(4)?,
        description: row.get(5)?,
        effect_kind: row.get(6)?,
        completion_type: row.get(7)?,
        proof_media_types: row.get(8)?,
        deadline_at: row.get(9)?,
        time_extension_seconds: row.get(10)?,
        time_reduction_seconds: row.get(11)?,
        proof_submission_id: row.get(12)?,
        on_success_template_id: row.get(13)?,
        on_failure_template_id: row.get(14)?,
        escalated_from_assignment_id: row.get(15)?,
        triggered_by_submission_id: row.get(16)?,
        triggered_by_play_session_id: row.get(17)?,
        spawned_by_recurring_task_rule_id: row.get(18)?,
        points_delta: row.get(19)?,
        assigned_at: row.get(20)?,
        assigned_by_user_id: row.get(21)?,
        assigned_via: row.get(22)?,
        status: row.get(23)?,
        notes: row.get(24)?,
    })
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Assignment>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM assignments WHERE id = ?1"),
        params![id],
        row_to_assignment,
    )
    .optional()
}

pub fn list_for_links(conn: &Connection, link_ids: &[String]) -> rusqlite::Result<Vec<Assignment>> {
    if link_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = link_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT {COLUMNS} FROM assignments WHERE link_id IN ({placeholders}) ORDER BY assigned_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = link_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();
    stmt.query_map(params.as_slice(), row_to_assignment)?
        .collect()
}

/// Walks the whole escalation chain an assignment belongs to: every
/// ancestor (following `escalated_from_assignment_id` backward) then the
/// assignment itself then every descendant (forward) — lets a Keyholder
/// see "this is link 3 of a chain that started with X"
/// (03-api-design.md §7).
pub fn chain(conn: &Connection, assignment_id: &str) -> rusqlite::Result<Vec<Assignment>> {
    let mut ancestors = Vec::new();
    let mut current_id = assignment_id.to_string();
    while let Some(a) = get(conn, &current_id)? {
        let parent = a.escalated_from_assignment_id.clone();
        ancestors.push(a);
        match parent {
            Some(p) => current_id = p,
            None => break,
        }
    }
    ancestors.reverse();

    let mut descendants = Vec::new();
    let mut frontier = vec![assignment_id.to_string()];
    while let Some(id) = frontier.pop() {
        let mut stmt =
            conn.prepare("SELECT id FROM assignments WHERE escalated_from_assignment_id = ?1")?;
        let children: Vec<String> = stmt
            .query_map(params![id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for child_id in children {
            if let Some(a) = get(conn, &child_id)? {
                frontier.push(a.id.clone());
                descendants.push(a);
            }
        }
    }

    ancestors.extend(descendants);
    Ok(ancestors)
}

#[derive(Debug, Error)]
pub enum CreateError {
    #[error("template not found")]
    TemplateNotFound,
    #[error("template is deactivated")]
    TemplateInactive,
    #[error("kind is required")]
    MissingKind,
    #[error("title is required")]
    MissingTitle,
    #[error("effect_kind is required for reward/punishment assignments")]
    MissingEffectKind,
    #[error("time_extension_seconds is required when effect_kind='time_extension'")]
    MissingTimeExtensionSeconds,
    #[error("time_reduction_seconds is required when effect_kind='time_reduction'")]
    MissingTimeReductionSeconds,
    #[error("completion_type is required for task assignments")]
    MissingCompletionType,
    #[error("proof_media_types is required when completion_type='proof_required'")]
    MissingProofMediaTypes,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Default)]
pub struct NewAssignment<'a> {
    pub kind: Option<&'a str>,
    pub template_id: Option<&'a str>,
    /// `true` for a fresh Keyholder-driven assignment (rejects an
    /// inactive template); `false` for an escalation, which must still
    /// honor a chain wired to a since-deactivated template
    /// (08-punishments-and-deadlines.md §6 step 1).
    pub require_active_template: bool,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub effect_kind: Option<&'a str>,
    pub time_extension_seconds: Option<i64>,
    pub time_reduction_seconds: Option<i64>,
    pub completion_type: Option<&'a str>,
    pub proof_media_types: Option<&'a str>,
    pub default_deadline_seconds: Option<i64>,
    pub deadline_at: Option<i64>,
    pub on_success_template_id: Option<&'a str>,
    pub on_failure_template_id: Option<&'a str>,
    pub points_delta: Option<i64>,
    pub notes: Option<&'a str>,
    pub triggered_by_submission_id: Option<&'a str>,
    pub triggered_by_play_session_id: Option<&'a str>,
    pub spawned_by_recurring_task_rule_id: Option<&'a str>,
    pub escalated_from_assignment_id: Option<&'a str>,
    pub assigned_by_user_id: Option<&'a str>,
    pub assigned_via: &'a str,
}

/// `POST /keyholder/submissives/{id}/assignments` (03-api-design.md §7)
/// and the internal escalation path (08-punishments-and-deadlines.md
/// §6/§6a) both funnel through here — the only difference between them
/// is which fields `NewAssignment` sets, not the logic itself.
pub fn create(
    conn: &Connection,
    submissive_id: &str,
    link_id: &str,
    new: NewAssignment,
) -> Result<Assignment, CreateError> {
    let template = match new.template_id {
        Some(tid) => {
            let t = templates::get(conn, tid)?.ok_or(CreateError::TemplateNotFound)?;
            if new.require_active_template && !t.active {
                return Err(CreateError::TemplateInactive);
            }
            Some(t)
        }
        None => None,
    };

    let kind = new
        .kind
        .map(str::to_string)
        .or_else(|| template.as_ref().map(|t| t.kind.clone()))
        .ok_or(CreateError::MissingKind)?;
    let title = new
        .title
        .map(str::to_string)
        .or_else(|| template.as_ref().map(|t| t.title.clone()))
        .ok_or(CreateError::MissingTitle)?;
    let description = new
        .description
        .map(str::to_string)
        .or_else(|| template.as_ref().and_then(|t| t.description.clone()));
    let effect_kind = new
        .effect_kind
        .map(str::to_string)
        .or_else(|| template.as_ref().and_then(|t| t.effect_kind.clone()));
    let completion_type = new
        .completion_type
        .map(str::to_string)
        .or_else(|| template.as_ref().and_then(|t| t.completion_type.clone()));
    let proof_media_types = new
        .proof_media_types
        .map(str::to_string)
        .or_else(|| template.as_ref().and_then(|t| t.proof_media_types.clone()));
    let time_extension_seconds = new
        .time_extension_seconds
        .or_else(|| template.as_ref().and_then(|t| t.time_extension_seconds));
    let time_reduction_seconds = new
        .time_reduction_seconds
        .or_else(|| template.as_ref().and_then(|t| t.time_reduction_seconds));
    let on_success_template_id = new.on_success_template_id.map(str::to_string).or_else(|| {
        template
            .as_ref()
            .and_then(|t| t.on_success_template_id.clone())
    });
    let on_failure_template_id = new.on_failure_template_id.map(str::to_string).or_else(|| {
        template
            .as_ref()
            .and_then(|t| t.on_failure_template_id.clone())
    });
    let points_delta = new
        .points_delta
        .or_else(|| template.as_ref().and_then(|t| t.points_delta));

    match kind.as_str() {
        "reward" | "punishment" => {
            let Some(effect_kind) = &effect_kind else {
                return Err(CreateError::MissingEffectKind);
            };
            if effect_kind == "time_extension" && time_extension_seconds.is_none() {
                return Err(CreateError::MissingTimeExtensionSeconds);
            }
            if effect_kind == "time_reduction" && time_reduction_seconds.is_none() {
                return Err(CreateError::MissingTimeReductionSeconds);
            }
        }
        "task" => {
            if completion_type.is_none() {
                return Err(CreateError::MissingCompletionType);
            }
            if completion_type.as_deref() == Some("proof_required") && proof_media_types.is_none() {
                return Err(CreateError::MissingProofMediaTypes);
            }
        }
        _ => {}
    }

    let ts = now();
    let deadline_at = if kind == "task" {
        Some(new.deadline_at.unwrap_or_else(|| {
            let secs = new
                .default_deadline_seconds
                .or_else(|| template.as_ref().and_then(|t| t.default_deadline_seconds))
                .unwrap_or(86_400);
            ts + secs
        }))
    } else {
        None
    };

    let is_immediate_effect = matches!(
        effect_kind.as_deref(),
        Some("time_extension") | Some("time_reduction")
    );
    let status = if is_immediate_effect {
        "applied"
    } else {
        "assigned"
    };

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO assignments
            (id, link_id, template_id, kind, title, description, effect_kind, completion_type,
             proof_media_types, deadline_at, time_extension_seconds, time_reduction_seconds,
             on_success_template_id, on_failure_template_id, escalated_from_assignment_id,
             triggered_by_submission_id, triggered_by_play_session_id, spawned_by_recurring_task_rule_id,
             points_delta, assigned_at,
             assigned_by_user_id, assigned_via, status, status_updated_at, notes)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25)",
        params![
            id,
            link_id,
            new.template_id,
            kind,
            title,
            description,
            effect_kind,
            completion_type,
            proof_media_types,
            deadline_at,
            time_extension_seconds,
            time_reduction_seconds,
            on_success_template_id,
            on_failure_template_id,
            new.escalated_from_assignment_id,
            new.triggered_by_submission_id,
            new.triggered_by_play_session_id,
            new.spawned_by_recurring_task_rule_id,
            points_delta,
            ts,
            new.assigned_by_user_id,
            new.assigned_via,
            status,
            ts,
            new.notes,
        ],
    )?;

    if is_immediate_effect {
        let (delta, reason) = if effect_kind.as_deref() == Some("time_extension") {
            (
                time_extension_seconds.unwrap_or(0),
                "punishment_time_extension",
            )
        } else {
            (
                -time_reduction_seconds.unwrap_or(0),
                "reward_time_reduction",
            )
        };
        confinement::apply_effect(
            conn,
            ApplyEffect {
                submissive_id,
                delta_seconds: delta,
                reason,
                caused_by_assignment_id: &id,
                adjusted_by_user_id: new.assigned_by_user_id,
                already_reviewed: new.assigned_via != "system",
            },
        )?;
    }

    Ok(get(conn, &id)?.expect("just inserted"))
}

fn submissive_id_for_link(conn: &Connection, link_id: &str) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT submissive_id FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| row.get(0),
    )
}

/// Escalation (08-punishments-and-deadlines.md §6/§6a): given a
/// just-resolved assignment and the template its resolution points to,
/// creates the next link in the chain. `T` is loaded even if
/// deactivated — deactivating only stops it being offered as a *new*
/// catalog choice, it doesn't retroactively sever an already-wired chain.
fn escalate(
    conn: &Connection,
    from: &Assignment,
    template_id: &str,
) -> Result<Assignment, CreateError> {
    let submissive_id = submissive_id_for_link(conn, &from.link_id)?;
    create(
        conn,
        &submissive_id,
        &from.link_id,
        NewAssignment {
            template_id: Some(template_id),
            require_active_template: false,
            escalated_from_assignment_id: Some(&from.id),
            assigned_via: "system",
            ..Default::default()
        },
    )
}

#[derive(Debug, Error)]
pub enum AcknowledgeError {
    #[error("nothing here for you to acknowledge")]
    NotAcknowledgeable,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PATCH /submissive/assignments/{id}/acknowledge` (03-api-design.md
/// §7) — `assigned` -> `acknowledged`. Covers both an `acknowledge_only`
/// task and a bare `effect_kind='grant'` reward/punishment
/// (01-data-model.md §6's state-machine section describes the grant
/// case going through the identical submissive-acknowledges step).
pub fn acknowledge(
    conn: &Connection,
    assignment_id: &str,
    submissive_id: &str,
) -> Result<(), AcknowledgeError> {
    let affected = conn.execute(
        "UPDATE assignments SET status = 'acknowledged', status_updated_at = ?1
         WHERE id = ?2 AND status = 'assigned'
           AND link_id IN (SELECT id FROM keyholder_submissive_links WHERE submissive_id = ?3)
           AND (
             (kind = 'task' AND completion_type = 'acknowledge_only')
             OR (kind IN ('reward', 'punishment') AND effect_kind = 'grant')
           )",
        params![now(), assignment_id, submissive_id],
    )?;
    if affected == 0 {
        return Err(AcknowledgeError::NotAcknowledgeable);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("assignment not found or not yours")]
    NotFound,
    #[error("assignment can't move to that status from where it is")]
    InvalidTransition,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PATCH /keyholder/assignments/{id}` `{status: "completed"|"revoked"}`
/// — for grants once acknowledged, and `acknowledge_only` tasks.
/// `proof_required` tasks resolve via proof review instead
/// (`review_task_proof` below), never through here. Returns the
/// escalated assignment, if `completed` triggered `on_success_template_id`
/// — the caller (API layer) uses this to fire the right notification
/// (09-notifications.md §3) without this function knowing about push.
pub fn resolve(
    conn: &mut Connection,
    assignment_id: &str,
    keyholder_id: &str,
    new_status: &str,
) -> Result<Option<Assignment>, ResolveError> {
    let tx = conn.transaction()?;

    type ResolveLookupRow = (String, String, String, Option<String>, Option<i64>);
    let row: Option<ResolveLookupRow> = tx
        .query_row(
            "SELECT a.link_id, a.status, a.kind, a.on_success_template_id, a.points_delta
             FROM assignments a JOIN keyholder_submissive_links l ON l.id = a.link_id
             WHERE a.id = ?1 AND l.keyholder_id = ?2",
            params![assignment_id, keyholder_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((link_id, status, kind, on_success_template_id, points_delta)) = row else {
        return Err(ResolveError::NotFound);
    };

    let allowed = match new_status {
        "completed" => status == "acknowledged",
        "revoked" => matches!(
            status.as_str(),
            "assigned" | "acknowledged" | "proof_submitted"
        ),
        _ => false,
    };
    if !allowed {
        return Err(ResolveError::InvalidTransition);
    }

    tx.execute(
        "UPDATE assignments SET status = ?1, status_updated_at = ?2 WHERE id = ?3",
        params![new_status, now(), assignment_id],
    )?;

    // Points (11-tasks-and-rewards.md §3) — task-only, and a no-op when
    // points aren't enabled for this link or the template set none.
    if new_status == "completed"
        && kind == "task"
        && let Some(delta) = points_delta
    {
        points::award_if_enabled(
            &tx,
            &link_id,
            delta,
            "task_completed",
            Some("assignments"),
            Some(assignment_id),
        )?;
    }

    let mut escalated = None;
    if new_status == "completed"
        && let Some(template_id) = on_success_template_id
    {
        let from = get(&tx, assignment_id)?.expect("just updated");
        escalated = Some(escalate(&tx, &from, &template_id).map_err(map_create_to_resolve_err)?);
    }

    tx.commit()?;
    Ok(escalated)
}

fn map_create_to_resolve_err(e: CreateError) -> ResolveError {
    match e {
        CreateError::Db(e) => ResolveError::Db(e),
        _ => ResolveError::Db(rusqlite::Error::InvalidQuery),
    }
}

#[derive(Debug, Error)]
pub enum SubmitProofError {
    #[error("assignment not found or not yours")]
    NotFound,
    #[error("this task doesn't take proof, or isn't awaiting it")]
    NotAwaitingProof,
    #[error("the deadline for this task has already passed")]
    DeadlinePassed,
    #[error(transparent)]
    Submit(#[from] proofs::SubmitError),
    #[error(transparent)]
    Store(#[from] crate::storage::StoreError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST /submissive/assignments/{id}/proof` (03-api-design.md §7) —
/// creates the `proof_submissions` row (`purpose='punishment_completion'`)
/// and moves the assignment to `proof_submitted`, atomically.
pub fn submit_proof(
    conn: &mut Connection,
    blob_dir: &std::path::Path,
    assignment_id: &str,
    submissive_id: &str,
    kind: &str,
    metadata: Option<&str>,
    files: Vec<proofs::NewFile>,
) -> Result<String, SubmitProofError> {
    let row: Option<(String, String, String, Option<i64>)> = conn
        .query_row(
            "SELECT a.link_id, a.completion_type, a.status, a.deadline_at
             FROM assignments a JOIN keyholder_submissive_links l ON l.id = a.link_id
             WHERE a.id = ?1 AND l.submissive_id = ?2 AND a.kind = 'task'",
            params![assignment_id, submissive_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get(2)?,
                    row.get(3)?,
                ))
            },
        )
        .optional()?;
    let Some((link_id, completion_type, status, deadline_at)) = row else {
        return Err(SubmitProofError::NotFound);
    };
    if completion_type != "proof_required" || status != "assigned" {
        return Err(SubmitProofError::NotAwaitingProof);
    }
    if deadline_at.is_some_and(|d| d < now()) {
        return Err(SubmitProofError::DeadlinePassed);
    }

    let mut stored = Vec::with_capacity(files.len());
    for file in &files {
        stored.push((
            crate::storage::store(blob_dir, file.content_type, file.bytes)?,
            file,
        ));
    }

    let submission_id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    let tx = conn.transaction()?;

    tx.execute(
        "INSERT INTO proof_submissions
            (id, submissive_id, link_id, purpose, assignment_id, kind, metadata, submitted_at)
         VALUES (?1, ?2, ?3, 'punishment_completion', ?4, ?5, ?6, ?7)",
        params![
            submission_id,
            submissive_id,
            link_id,
            assignment_id,
            kind,
            metadata,
            ts
        ],
    )?;
    for (stored_file, file) in &stored {
        tx.execute(
            "INSERT INTO proof_attachments
                (id, submission_id, storage_path, original_filename, mime_type, byte_size, sha256, uploaded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                uuid::Uuid::new_v4().to_string(),
                submission_id,
                stored_file.storage_path,
                file.original_filename,
                file.content_type,
                stored_file.byte_size,
                stored_file.sha256,
                ts,
            ],
        )?;
    }
    tx.execute(
        "UPDATE assignments SET status = 'proof_submitted', status_updated_at = ?1, proof_submission_id = ?2
         WHERE id = ?3",
        params![ts, submission_id, assignment_id],
    )?;

    tx.commit()?;
    Ok(submission_id)
}

#[derive(Debug, Error)]
pub enum ReviewProofError {
    #[error("submission not reviewable")]
    Proof(#[from] proofs::ReviewError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// Outcome of a task-completion proof review, for the caller (API
/// layer) to decide what notification(s) to fire
/// (09-notifications.md §3) — this function itself stays
/// notification-agnostic.
pub struct ReviewProofOutcome {
    /// Set when the reviewed submission was `purpose='punishment_completion'`
    /// — the task/punishment assignment it resolved.
    pub resolved_assignment: Option<Assignment>,
    /// Set when that resolution triggered an escalation
    /// (`on_success_template_id`/`on_failure_template_id`).
    pub escalated: Option<Assignment>,
}

/// The single review endpoint both verification and task-completion
/// proof share (04-verification-workflow.md §§4, 7): does the ordinary
/// submission review, then — only when `purpose='punishment_completion'`
/// — the extra assignment-side transition and escalation.
pub fn review_proof(
    conn: &mut Connection,
    submission_id: &str,
    link_id: &str,
    status: &str,
    review_notes: Option<&str>,
    reviewed_by_user_id: &str,
    reviewed_via: &str,
) -> Result<ReviewProofOutcome, ReviewProofError> {
    let tx = conn.transaction()?;

    proofs::review(
        &tx,
        submission_id,
        link_id,
        status,
        review_notes,
        reviewed_by_user_id,
        reviewed_via,
    )?;

    let mut outcome = ReviewProofOutcome {
        resolved_assignment: None,
        escalated: None,
    };

    let submission = proofs::get(&tx, submission_id)?;
    if let Some(submission) = submission
        && submission.purpose == "punishment_completion"
        && let Some(assignment_id) = &submission.assignment_id
        && let Some(assignment) = get(&tx, assignment_id)?
    {
        match status {
            "verified" => {
                tx.execute(
                    "UPDATE assignments SET status = 'completed', status_updated_at = ?1 WHERE id = ?2",
                    params![now(), assignment_id],
                )?;
                if let Some(delta) = assignment.points_delta {
                    points::award_if_enabled(
                        &tx,
                        link_id,
                        delta,
                        "task_completed",
                        Some("assignments"),
                        Some(assignment_id),
                    )?;
                }
                let updated = get(&tx, assignment_id)?.expect("just updated");
                if let Some(template_id) = &assignment.on_success_template_id {
                    outcome.escalated =
                        Some(escalate(&tx, &updated, template_id).map_err(|e| match e {
                            CreateError::Db(e) => ReviewProofError::Db(e),
                            _ => ReviewProofError::Db(rusqlite::Error::InvalidQuery),
                        })?);
                }
                outcome.resolved_assignment = Some(updated);
            }
            "failed" => {
                tx.execute(
                    "UPDATE assignments SET status = 'failed', status_updated_at = ?1 WHERE id = ?2",
                    params![now(), assignment_id],
                )?;
                audit::record(
                    &tx,
                    audit::Entry {
                        actor: audit::Actor::User(reviewed_by_user_id),
                        link_id: Some(link_id),
                        action: "assignment.failed",
                        entity_type: "assignments",
                        entity_id: assignment_id,
                        detail: None,
                    },
                )?;
                if let Some(delta) = assignment.points_delta {
                    points::award_if_enabled(
                        &tx,
                        link_id,
                        delta,
                        "task_failed",
                        Some("assignments"),
                        Some(assignment_id),
                    )?;
                }
                let updated = get(&tx, assignment_id)?.expect("just updated");
                if let Some(template_id) = &assignment.on_failure_template_id {
                    outcome.escalated =
                        Some(escalate(&tx, &updated, template_id).map_err(|e| match e {
                            CreateError::Db(e) => ReviewProofError::Db(e),
                            _ => ReviewProofError::Db(rusqlite::Error::InvalidQuery),
                        })?);
                }
                outcome.resolved_assignment = Some(updated);
            }
            _ => {} // "redo": assignment stays proof_submitted-awaiting-resubmission logically, but
                    // stays in whatever status it's in — nothing to change here.
        }
    }

    tx.commit()?;
    Ok(outcome)
}

#[derive(Debug, Error)]
pub enum EditError {
    #[error("assignment not found or not yours")]
    NotFound,
    #[error("assignment has already resolved — nothing left to edit")]
    AlreadyResolved,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PATCH /keyholder/assignments/{id}/deadline` — an absolute set,
/// unlike the confinement timer's delta-only pattern, since a deadline
/// is a single point in time (03-api-design.md §7).
pub fn edit_deadline(
    conn: &Connection,
    assignment_id: &str,
    keyholder_id: &str,
    new_deadline_at: i64,
) -> Result<(), EditError> {
    let status: Option<String> = conn
        .query_row(
            "SELECT a.status FROM assignments a JOIN keyholder_submissive_links l ON l.id = a.link_id
             WHERE a.id = ?1 AND l.keyholder_id = ?2",
            params![assignment_id, keyholder_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(status) = status else {
        return Err(EditError::NotFound);
    };
    if !matches!(status.as_str(), "assigned" | "proof_submitted") {
        return Err(EditError::AlreadyResolved);
    }
    conn.execute(
        "UPDATE assignments SET deadline_at = ?1 WHERE id = ?2",
        params![new_deadline_at, assignment_id],
    )?;
    Ok(())
}

/// `PATCH /keyholder/assignments/{id}/escalation` `{on_failure_template_id}`
/// — reconsider the consequence after assigning, without revoking and
/// recreating from scratch. Same open-window rule as the deadline edit.
pub fn edit_escalation(
    conn: &Connection,
    assignment_id: &str,
    keyholder_id: &str,
    on_failure_template_id: Option<&str>,
) -> Result<(), EditError> {
    let status: Option<String> = conn
        .query_row(
            "SELECT a.status FROM assignments a JOIN keyholder_submissive_links l ON l.id = a.link_id
             WHERE a.id = ?1 AND l.keyholder_id = ?2",
            params![assignment_id, keyholder_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(status) = status else {
        return Err(EditError::NotFound);
    };
    if !matches!(status.as_str(), "assigned" | "proof_submitted") {
        return Err(EditError::AlreadyResolved);
    }
    conn.execute(
        "UPDATE assignments SET on_failure_template_id = ?1 WHERE id = ?2",
        params![on_failure_template_id, assignment_id],
    )?;
    Ok(())
}

/// One task auto-failed on its deadline this tick, for the caller (the
/// sweeper's async wrapper in `main.rs`) to notify both parties.
pub struct AutoFailedTask {
    pub assignment_id: String,
    pub link_id: String,
    pub keyholder_id: String,
    pub submissive_id: String,
    /// Set when `on_failure_template_id` was configured — the new
    /// assignment the failure escalated into.
    pub escalated: Option<Assignment>,
}

/// One task whose deadline-approaching reminder (08-punishments-and-
/// deadlines.md §3 step 2) is due this tick.
pub struct DeadlineReminder {
    pub assignment_id: String,
    pub submissive_id: String,
    pub title: String,
}

pub struct SweepOutcome {
    pub auto_failed: Vec<AutoFailedTask>,
    pub reminders: Vec<DeadlineReminder>,
}

/// The deadline sweeper's full tick (08-punishments-and-deadlines.md
/// §3): the auto-fail pass (step 1), then the purely-informational
/// deadline-approaching pass (step 2). Notification-agnostic by
/// design — returns what happened, the caller (`main.rs`'s async
/// wrapper, which has a `Pool` and can spawn push sends) decides what
/// to notify.
pub fn run_deadline_sweep_tick(conn: &mut Connection) -> rusqlite::Result<SweepOutcome> {
    let overdue: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM assignments WHERE kind = 'task' AND status = 'assigned' AND deadline_at < ?1",
        )?;
        stmt.query_map(params![now()], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };

    let mut auto_failed = Vec::new();
    for assignment_id in overdue {
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE assignments SET status = 'failed', status_updated_at = ?1 WHERE id = ?2 AND status = 'assigned'",
            params![now(), assignment_id],
        )?;
        if tx.changes() == 0 {
            // Raced with something else since the SELECT above — skip.
            tx.commit()?;
            continue;
        }
        audit::record(
            &tx,
            audit::Entry {
                actor: audit::Actor::System,
                link_id: None,
                action: "assignment.auto_failed",
                entity_type: "assignments",
                entity_id: &assignment_id,
                detail: None,
            },
        )?;

        let Some(assignment) = get(&tx, &assignment_id)? else {
            tx.commit()?;
            continue;
        };
        if let Some(delta) = assignment.points_delta {
            points::award_if_enabled(
                &tx,
                &assignment.link_id,
                delta,
                "task_failed",
                Some("assignments"),
                Some(&assignment_id),
            )?;
        }
        let Some((keyholder_id, submissive_id)) =
            crate::domain::links::parties(&tx, &assignment.link_id)?
        else {
            tx.commit()?;
            continue;
        };
        let mut escalated = None;
        if let Some(template_id) = &assignment.on_failure_template_id {
            escalated = Some(
                escalate(&tx, &assignment, template_id).map_err(|e| match e {
                    CreateError::Db(e) => e,
                    _ => rusqlite::Error::InvalidQuery,
                })?,
            );
        }
        tx.commit()?;
        auto_failed.push(AutoFailedTask {
            assignment_id,
            link_id: assignment.link_id,
            keyholder_id,
            submissive_id,
            escalated,
        });
    }

    // Deadline-approaching pass — see 08-punishments-and-deadlines.md
    // §3 step 2 for the window-scales-with-length reasoning and the
    // dedupe-via-`notifications` approach.
    let now_ts = now();
    let candidates: Vec<(String, i64, i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT a.id, a.deadline_at, a.assigned_at, a.title, l.submissive_id
             FROM assignments a JOIN keyholder_submissive_links l ON l.id = a.link_id
             WHERE a.kind = 'task' AND a.status = 'assigned' AND a.deadline_at IS NOT NULL",
        )?;
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };

    let mut reminders = Vec::new();
    for (assignment_id, deadline_at, assigned_at, title, submissive_id) in candidates {
        let total = deadline_at - assigned_at;
        let window = std::cmp::min(3600, total / 2);
        let reminder_at = deadline_at - window;
        // A reminder landing less than 5 minutes after assignment is no
        // reminder at all for a punishment this short — the assignment
        // notification itself is the warning (08-punishments-and-
        // deadlines.md §3 step 2).
        if reminder_at - assigned_at < 300 {
            continue;
        }
        if now_ts < reminder_at {
            continue;
        }
        if crate::domain::notifications::exists_for_related_entity(
            conn,
            "task.deadline_approaching",
            &assignment_id,
        )? {
            continue;
        }
        reminders.push(DeadlineReminder {
            assignment_id,
            submissive_id,
            title,
        });
    }

    Ok(SweepOutcome {
        auto_failed,
        reminders,
    })
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

    #[test]
    fn ad_hoc_task_assignment_computes_deadline_from_seconds() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        let before = now();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("500 lines"),
                completion_type: Some("acknowledge_only"),
                default_deadline_seconds: Some(3600),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(a.status, "assigned");
        assert!(a.deadline_at.unwrap() >= before + 3600);
    }

    #[test]
    fn task_without_completion_type_is_rejected() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        let result = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("x"),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        );
        assert!(matches!(result, Err(CreateError::MissingCompletionType)));
    }

    #[test]
    fn time_extension_punishment_applies_immediately_and_is_terminal() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        let device_id =
            crate::domain::chastity::devices::add(&conn, &submissive_id, "cage", None).unwrap();
        confinement::start(
            &conn,
            confinement::StartSession {
                submissive_id: &submissive_id,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: Some(1000),
                notes: None,
            },
        )
        .unwrap();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("punishment"),
                title: Some("extra day locked"),
                effect_kind: Some("time_extension"),
                time_extension_seconds: Some(500),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(a.status, "applied");
        let session = confinement::current(&conn, &submissive_id)
            .unwrap()
            .unwrap();
        assert_eq!(session.target_release_at, Some(1500));
    }

    #[test]
    fn escalation_on_deadline_sweep_creates_the_next_link() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let punishment_template = templates::create(
            &conn,
            &keyholder_id,
            templates::NewTemplate {
                kind: "punishment",
                title: "extra day locked",
                description: None,
                severity: None,
                effect_kind: Some("time_extension"),
                completion_type: None,
                proof_media_types: None,
                default_deadline_seconds: None,
                time_extension_seconds: Some(86_400),
                time_reduction_seconds: None,
                on_success_template_id: None,
                on_failure_template_id: None,
                points_delta: None,
                points_cost: None,
            },
        )
        .unwrap();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("cold shower"),
                completion_type: Some("acknowledge_only"),
                deadline_at: Some(now() - 10), // already overdue
                on_failure_template_id: Some(&punishment_template),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        let outcome = run_deadline_sweep_tick(&mut conn).unwrap();
        assert_eq!(outcome.auto_failed.len(), 1);
        assert_eq!(outcome.auto_failed[0].assignment_id, a.id);
        assert_eq!(outcome.auto_failed[0].submissive_id, submissive_id);
        assert_eq!(outcome.auto_failed[0].keyholder_id, keyholder_id);
        let escalated_outcome = outcome.auto_failed[0].escalated.as_ref().unwrap();
        assert_eq!(escalated_outcome.kind, "punishment");

        let original = get(&conn, &a.id).unwrap().unwrap();
        assert_eq!(original.status, "failed");

        let escalated: Vec<Assignment> = list_for_links(&conn, std::slice::from_ref(&link_id))
            .unwrap()
            .into_iter()
            .filter(|x| x.escalated_from_assignment_id.as_deref() == Some(a.id.as_str()))
            .collect();
        assert_eq!(escalated.len(), 1);
        assert_eq!(escalated[0].kind, "punishment");
        assert_eq!(escalated[0].status, "applied");
        assert_eq!(escalated[0].assigned_via, "system");
    }

    #[test]
    fn sweep_does_not_touch_acknowledged_or_proof_submitted() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("cold shower"),
                completion_type: Some("acknowledge_only"),
                deadline_at: Some(now() - 10),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();
        acknowledge(&conn, &a.id, &submissive_id).unwrap();

        let outcome = run_deadline_sweep_tick(&mut conn).unwrap();
        assert_eq!(outcome.auto_failed.len(), 0);
        assert_eq!(get(&conn, &a.id).unwrap().unwrap().status, "acknowledged");
    }

    #[test]
    fn sweep_sends_exactly_one_deadline_approaching_reminder() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("clean the apartment"),
                completion_type: Some("acknowledge_only"),
                deadline_at: Some(now() + 100_000), // placeholder, backdated below
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        // A 1-hour task whose reminder window (min(1h, total/2) = 30min)
        // is already due: assigned an hour ago, due in 100s.
        conn.execute(
            "UPDATE assignments SET assigned_at = ?1, deadline_at = ?2 WHERE id = ?3",
            params![now() - 3500, now() + 100, a.id],
        )
        .unwrap();

        let outcome = run_deadline_sweep_tick(&mut conn).unwrap();
        assert_eq!(outcome.reminders.len(), 1);
        assert_eq!(outcome.reminders[0].assignment_id, a.id);
        assert_eq!(outcome.reminders[0].submissive_id, submissive_id);

        // The caller (main.rs) would write a `notifications` row here —
        // simulate that, then confirm the next tick doesn't repeat it.
        crate::domain::notifications::create(
            &conn,
            crate::domain::notifications::NewNotification {
                user_id: &submissive_id,
                link_id: None,
                notification_type: "task.deadline_approaching",
                title: "reminder",
                body: None,
                link_path: None,
                related_entity_type: Some("assignments"),
                related_entity_id: Some(&a.id),
            },
        )
        .unwrap();

        let outcome = run_deadline_sweep_tick(&mut conn).unwrap();
        assert_eq!(outcome.reminders.len(), 0);
    }

    #[test]
    fn sweep_skips_a_reminder_for_a_punishment_too_short_to_bother() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("cold shower"),
                completion_type: Some("acknowledge_only"),
                deadline_at: Some(now() + 100_000),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        // total = 250s, window = min(1h, 125s) = 125s, reminder_at -
        // assigned_at = 125s < the 300s floor — no reminder is ever
        // scheduled for a punishment this short (the assignment
        // notification itself is the warning). Deadline is still in
        // the future, so this isn't the auto-fail path masking it.
        conn.execute(
            "UPDATE assignments SET assigned_at = ?1, deadline_at = ?2 WHERE id = ?3",
            params![now() - 50, now() + 200, a.id],
        )
        .unwrap();

        let outcome = run_deadline_sweep_tick(&mut conn).unwrap();
        assert!(outcome.auto_failed.is_empty());
        assert!(outcome.reminders.is_empty());
    }

    #[test]
    fn acknowledge_then_resolve_completed_triggers_success_escalation() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let reward_template = templates::create(
            &conn,
            &keyholder_id,
            templates::NewTemplate {
                kind: "reward",
                title: "movie night",
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

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("clean the apartment"),
                completion_type: Some("acknowledge_only"),
                default_deadline_seconds: Some(3600),
                on_success_template_id: Some(&reward_template),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        acknowledge(&conn, &a.id, &submissive_id).unwrap();
        resolve(&mut conn, &a.id, &keyholder_id, "completed").unwrap();

        assert_eq!(get(&conn, &a.id).unwrap().unwrap().status, "completed");
        let rewards: Vec<Assignment> = list_for_links(&conn, std::slice::from_ref(&link_id))
            .unwrap()
            .into_iter()
            .filter(|x| x.kind == "reward")
            .collect();
        assert_eq!(rewards.len(), 1);
        assert_eq!(
            rewards[0].escalated_from_assignment_id.as_deref(),
            Some(a.id.as_str())
        );
    }

    #[test]
    fn revoke_never_escalates() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let punishment_template = templates::create(
            &conn,
            &keyholder_id,
            templates::NewTemplate {
                kind: "punishment",
                title: "extra day locked",
                description: None,
                severity: None,
                effect_kind: Some("time_extension"),
                completion_type: None,
                proof_media_types: None,
                default_deadline_seconds: None,
                time_extension_seconds: Some(3600),
                time_reduction_seconds: None,
                on_success_template_id: None,
                on_failure_template_id: None,
                points_delta: None,
                points_cost: None,
            },
        )
        .unwrap();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("cold shower"),
                completion_type: Some("acknowledge_only"),
                default_deadline_seconds: Some(3600),
                on_failure_template_id: Some(&punishment_template),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        resolve(&mut conn, &a.id, &keyholder_id, "revoked").unwrap();
        assert_eq!(get(&conn, &a.id).unwrap().unwrap().status, "revoked");
        assert!(
            list_for_links(&conn, &[link_id])
                .unwrap()
                .iter()
                .all(|x| x.kind != "punishment")
        );
    }

    #[test]
    fn proof_required_task_flow_end_to_end_with_review() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let blob_dir = tempfile::tempdir().unwrap();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("cold shower"),
                completion_type: Some("proof_required"),
                proof_media_types: Some(r#"["video"]"#),
                default_deadline_seconds: Some(3600),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        let submission_id = submit_proof(
            &mut conn,
            blob_dir.path(),
            &a.id,
            &submissive_id,
            "video",
            None,
            vec![],
        )
        .unwrap();
        assert_eq!(
            get(&conn, &a.id).unwrap().unwrap().status,
            "proof_submitted"
        );

        review_proof(
            &mut conn,
            &submission_id,
            &link_id,
            "verified",
            None,
            &keyholder_id,
            "session",
        )
        .unwrap();
        assert_eq!(get(&conn, &a.id).unwrap().unwrap().status, "completed");
    }

    #[test]
    fn proof_rejected_fails_the_task_and_escalates() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();
        let blob_dir = tempfile::tempdir().unwrap();

        let punishment_template = templates::create(
            &conn,
            &keyholder_id,
            templates::NewTemplate {
                kind: "punishment",
                title: "extra day locked",
                description: None,
                severity: None,
                effect_kind: Some("time_extension"),
                completion_type: None,
                proof_media_types: None,
                default_deadline_seconds: None,
                time_extension_seconds: Some(3600),
                time_reduction_seconds: None,
                on_success_template_id: None,
                on_failure_template_id: None,
                points_delta: None,
                points_cost: None,
            },
        )
        .unwrap();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("cold shower"),
                completion_type: Some("proof_required"),
                proof_media_types: Some(r#"["video"]"#),
                default_deadline_seconds: Some(3600),
                on_failure_template_id: Some(&punishment_template),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        let submission_id = submit_proof(
            &mut conn,
            blob_dir.path(),
            &a.id,
            &submissive_id,
            "video",
            None,
            vec![],
        )
        .unwrap();
        review_proof(
            &mut conn,
            &submission_id,
            &link_id,
            "failed",
            Some("not real"),
            &keyholder_id,
            "session",
        )
        .unwrap();

        assert_eq!(get(&conn, &a.id).unwrap().unwrap().status, "failed");
        let escalated: Vec<Assignment> = list_for_links(&conn, &[link_id])
            .unwrap()
            .into_iter()
            .filter(|x| x.escalated_from_assignment_id.as_deref() == Some(a.id.as_str()))
            .collect();
        assert_eq!(escalated.len(), 1);
        assert_eq!(escalated[0].kind, "punishment");
    }

    #[test]
    fn deadline_and_escalation_edits_are_rejected_once_resolved() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();

        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("x"),
                completion_type: Some("acknowledge_only"),
                default_deadline_seconds: Some(3600),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();

        acknowledge(&conn, &a.id, &submissive_id).unwrap();
        let mut conn_mut = conn;
        resolve(&mut conn_mut, &a.id, &keyholder_id, "completed").unwrap();

        let result = edit_deadline(&conn_mut, &a.id, &keyholder_id, now() + 1000);
        assert!(matches!(result, Err(EditError::AlreadyResolved)));
        let result = edit_escalation(&conn_mut, &a.id, &keyholder_id, None);
        assert!(matches!(result, Err(EditError::AlreadyResolved)));
    }

    #[test]
    fn chain_walks_ancestors_and_descendants() {
        let (_dir, pool, keyholder_id, submissive_id, link_id) = temp_pool_with_link();
        let mut conn = pool.get().unwrap();

        let punishment_template = templates::create(
            &conn,
            &keyholder_id,
            templates::NewTemplate {
                kind: "punishment",
                title: "extra day locked",
                description: None,
                severity: None,
                effect_kind: Some("time_extension"),
                completion_type: None,
                proof_media_types: None,
                default_deadline_seconds: None,
                time_extension_seconds: Some(3600),
                time_reduction_seconds: None,
                on_success_template_id: None,
                on_failure_template_id: None,
                points_delta: None,
                points_cost: None,
            },
        )
        .unwrap();
        let a = create(
            &conn,
            &submissive_id,
            &link_id,
            NewAssignment {
                kind: Some("task"),
                title: Some("cold shower"),
                completion_type: Some("acknowledge_only"),
                deadline_at: Some(now() - 10),
                on_failure_template_id: Some(&punishment_template),
                assigned_by_user_id: Some(&keyholder_id),
                assigned_via: "session",
                ..Default::default()
            },
        )
        .unwrap();
        run_deadline_sweep_tick(&mut conn).unwrap();

        let full_chain = chain(&conn, &a.id).unwrap();
        // The original task plus the escalated punishment.
        assert_eq!(full_chain.len(), 2);
        assert!(full_chain.iter().any(|x| x.id == a.id));
        assert!(full_chain.iter().any(|x| x.kind == "punishment"));
    }
}
