//! `keyholder_submissive_links` (01-data-model.md §3) — the join table
//! establishing ownership. Creating a link always creates its default
//! `verification_policies` row in the same transaction (01-data-model.md
//! §5), so there's never an undefined window before a Keyholder
//! configures a real schedule.

use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::audit;

const DEFAULT_CODE_TTL_SECS: i64 = 15 * 60;
const DEFAULT_GRACE_PERIOD_SECS: i64 = 10 * 60;

/// The caller's own active link — every submissive-role query is
/// implicitly scoped to this (02-roles-and-permissions.md §1 principle
/// 3).
pub fn active_link_for_submissive(
    conn: &Connection,
    submissive_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM keyholder_submissive_links
         WHERE submissive_id = ?1 AND status = 'active'",
        params![submissive_id],
        |row| row.get(0),
    )
    .optional()
}

/// Like `active_link_for_submissive`, but also reaches a `paused`
/// link — needed for the end-request flow (06-future-extensions.md
/// §2), which stays reachable through a pause the same way it stays
/// reachable through one on the Keyholder side
/// (`active_or_paused_link_for_keyholder`).
pub fn active_or_paused_link_for_submissive(
    conn: &Connection,
    submissive_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM keyholder_submissive_links
         WHERE submissive_id = ?1 AND status IN ('active', 'paused')",
        params![submissive_id],
        |row| row.get(0),
    )
    .optional()
}

/// Every `active` link id for a Keyholder — the cross-roster feed's
/// scoping join (03-api-design.md §6, 02-roles-and-permissions.md §5).
pub fn active_link_ids_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM keyholder_submissive_links WHERE keyholder_id = ?1 AND status = 'active'",
    )?;
    stmt.query_map(params![keyholder_id], |row| row.get(0))?
        .collect()
}

/// Resolves `submissive_id` to the caller's own link to them, or `None`
/// if no such link exists — the join every Keyholder-role query must go
/// through rather than trusting a client-supplied submissive id
/// (02-roles-and-permissions.md §1 principle 2). Includes `paused` links
/// (a Keyholder still has read access to those, per that same principle)
/// but not `ended` ones, since none of Phase 2's write actions should be
/// reachable against a relationship that's over.
pub fn active_or_paused_link_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
    submissive_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM keyholder_submissive_links
         WHERE keyholder_id = ?1 AND submissive_id = ?2 AND status IN ('active', 'paused')",
        params![keyholder_id, submissive_id],
        |row| row.get(0),
    )
    .optional()
}

/// Creates an `active` link between a Keyholder and a submissive plus its
/// default on-demand-only verification policy. Callers are expected to run
/// this inside a transaction alongside whatever else the moment needs
/// (e.g. invite redemption also creates the submissive account).
pub fn create(
    conn: &Connection,
    keyholder_id: &str,
    submissive_id: &str,
) -> rusqlite::Result<String> {
    let link_id = uuid::Uuid::new_v4().to_string();
    let now = crate::auth::session::now();

    conn.execute(
        "INSERT INTO keyholder_submissive_links (id, keyholder_id, submissive_id, status, started_at)
         VALUES (?1, ?2, ?3, 'active', ?4)",
        params![link_id, keyholder_id, submissive_id, now],
    )?;

    conn.execute(
        "INSERT INTO verification_policies
            (id, link_id, frequency_kind, frequency_value, code_ttl_seconds, grace_period_seconds, created_at, updated_at)
         VALUES (?1, ?2, 'on_demand_only', '{}', ?3, ?4, ?5, ?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            link_id,
            DEFAULT_CODE_TTL_SECS,
            DEFAULT_GRACE_PERIOD_SECS,
            now,
        ],
    )?;

    Ok(link_id)
}

/// `admin force-end-link <link_id>` (10-operations.md §5,
/// 06-future-extensions.md §2) — the Tier 2 escape hatch for a Keyholder
/// who never responds to an end-link request at all. Ends an `active` or
/// `paused` link unilaterally; returns `false` if no such link exists to
/// end (already ended, or the id doesn't exist).
pub fn force_end(conn: &Connection, link_id: &str) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "UPDATE keyholder_submissive_links SET status = 'ended', ended_at = ?1
         WHERE id = ?2 AND status IN ('active', 'paused')",
        params![crate::auth::session::now(), link_id],
    )?;
    Ok(affected > 0)
}

#[derive(Debug, thiserror::Error)]
pub enum RequestEndError {
    #[error("no active or paused link")]
    NoLink,
    #[error("a request is already pending")]
    AlreadyPending,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST /submissive/link/end-request` (06-future-extensions.md §2) —
/// a request, not an action: doesn't itself change `status`, doesn't
/// touch anything else operative about the link. Returns the link id
/// so the caller can resolve the Keyholder to notify.
pub fn request_end(
    conn: &Connection,
    submissive_id: &str,
    reason: Option<&str>,
) -> Result<String, RequestEndError> {
    let Some(link_id) = active_or_paused_link_for_submissive(conn, submissive_id)? else {
        return Err(RequestEndError::NoLink);
    };
    let already_pending: bool = conn.query_row(
        "SELECT end_requested_at IS NOT NULL FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| row.get(0),
    )?;
    if already_pending {
        return Err(RequestEndError::AlreadyPending);
    }
    conn.execute(
        "UPDATE keyholder_submissive_links
         SET end_requested_at = ?1, end_requested_by_user_id = ?2, end_request_reason = ?3,
             end_request_escalated_at = NULL
         WHERE id = ?4",
        params![crate::auth::session::now(), submissive_id, reason, link_id],
    )?;
    Ok(link_id)
}

/// `DELETE /submissive/link/end-request` — the submissive withdraws
/// their own request at any time, no confirmation from the Keyholder
/// needed. Returns the link id if there was something to withdraw,
/// `None` if there wasn't (no link, or nothing pending) — either way
/// a no-op the caller can treat as success.
pub fn withdraw_end_request(
    conn: &Connection,
    submissive_id: &str,
) -> rusqlite::Result<Option<String>> {
    let Some(link_id) = active_or_paused_link_for_submissive(conn, submissive_id)? else {
        return Ok(None);
    };
    let affected = conn.execute(
        "UPDATE keyholder_submissive_links
         SET end_requested_at = NULL, end_requested_by_user_id = NULL,
             end_request_reason = NULL, end_request_escalated_at = NULL
         WHERE id = ?1 AND end_requested_at IS NOT NULL",
        params![link_id],
    )?;
    Ok(if affected > 0 { Some(link_id) } else { None })
}

#[derive(Debug, thiserror::Error)]
pub enum DeclineEndRequestError {
    #[error("no such link, or nothing pending on it")]
    NotFound,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST /keyholder/submissives/{id}/link/end-request/decline` —
/// clears the request without ending the link; audit-logged so a
/// dismissed request is never just silently gone. Declining isn't
/// final — a fresh `request_end` can always reopen it. Returns the
/// submissive id so the caller can notify them, `response_note` and
/// all.
pub fn decline_end_request(
    conn: &Connection,
    link_id: &str,
    keyholder_id: &str,
) -> Result<String, DeclineEndRequestError> {
    let submissive_id: Option<String> = conn
        .query_row(
            "SELECT submissive_id FROM keyholder_submissive_links
             WHERE id = ?1 AND keyholder_id = ?2 AND end_requested_at IS NOT NULL",
            params![link_id, keyholder_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(submissive_id) = submissive_id else {
        return Err(DeclineEndRequestError::NotFound);
    };
    conn.execute(
        "UPDATE keyholder_submissive_links
         SET end_requested_at = NULL, end_requested_by_user_id = NULL,
             end_request_reason = NULL, end_request_escalated_at = NULL
         WHERE id = ?1",
        params![link_id],
    )?;
    audit::record(
        conn,
        audit::Entry {
            actor: audit::Actor::User(keyholder_id),
            link_id: Some(link_id),
            action: "link.end_request_declined",
            entity_type: "keyholder_submissive_links",
            entity_id: link_id,
            detail: None,
        },
    )?;
    Ok(submissive_id)
}

pub struct PendingEndRequest {
    pub link_id: String,
    pub keyholder_id: String,
    pub submissive_id: String,
    pub submissive_display_name: String,
    pub requested_at: i64,
    pub reason: Option<String>,
    pub escalated_at: Option<i64>,
}

/// Every link with a pending end-request that this Keyholder owns —
/// the read side of the whole flow (the decline endpoint, and the
/// "impossible to miss on every page load" banner once escalated).
pub fn pending_end_requests_for_keyholder(
    conn: &Connection,
    keyholder_id: &str,
) -> rusqlite::Result<Vec<PendingEndRequest>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, l.keyholder_id, l.submissive_id, u.display_name, l.end_requested_at,
                l.end_request_reason, l.end_request_escalated_at
         FROM keyholder_submissive_links l
         JOIN users u ON u.id = l.submissive_id
         WHERE l.keyholder_id = ?1 AND l.end_requested_at IS NOT NULL
         ORDER BY l.end_requested_at ASC",
    )?;
    stmt.query_map(params![keyholder_id], |row| {
        Ok(PendingEndRequest {
            link_id: row.get(0)?,
            keyholder_id: row.get(1)?,
            submissive_id: row.get(2)?,
            submissive_display_name: row.get(3)?,
            requested_at: row.get(4)?,
            reason: row.get(5)?,
            escalated_at: row.get(6)?,
        })
    })?
    .collect()
}

/// The submissive-facing read side — their own link's pending
/// request, if any, so the account page can render its current state
/// on load rather than only reactively after they just submitted one.
pub fn own_pending_end_request(
    conn: &Connection,
    submissive_id: &str,
) -> rusqlite::Result<Option<PendingEndRequest>> {
    conn.query_row(
        "SELECT l.id, l.keyholder_id, l.submissive_id, u.display_name, l.end_requested_at,
                l.end_request_reason, l.end_request_escalated_at
         FROM keyholder_submissive_links l
         JOIN users u ON u.id = l.submissive_id
         WHERE l.submissive_id = ?1 AND l.end_requested_at IS NOT NULL",
        params![submissive_id],
        |row| {
            Ok(PendingEndRequest {
                link_id: row.get(0)?,
                keyholder_id: row.get(1)?,
                submissive_id: row.get(2)?,
                submissive_display_name: row.get(3)?,
                requested_at: row.get(4)?,
                reason: row.get(5)?,
                escalated_at: row.get(6)?,
            })
        },
    )
    .optional()
}

const END_REQUEST_ESCALATION_THRESHOLD_SECS: i64 = 7 * 24 * 3600;
const END_REQUEST_REMINDER_INTERVAL_SECS: i64 = 24 * 3600;

/// Runs on the same tick as the deadline sweeper
/// (08-punishments-and-deadlines.md §9's reasoning for not warranting
/// a third background task): every link with a request pending 7
/// days or more gets `end_request_escalated_at` set (once), and every
/// already-escalated link that hasn't had a
/// `link.end_request_reminder` notification in the last 24h is
/// reported again — the caller fires that same notification type
/// either way, since "just crossed the 7-day mark" and "still
/// escalated a day later" both read as the same reminder to a
/// Keyholder who hasn't acted.
pub fn run_end_request_escalation_sweep_tick(
    conn: &Connection,
) -> rusqlite::Result<Vec<PendingEndRequest>> {
    let now_ts = crate::auth::session::now();
    conn.execute(
        "UPDATE keyholder_submissive_links
         SET end_request_escalated_at = ?1
         WHERE end_requested_at IS NOT NULL AND end_request_escalated_at IS NULL
           AND end_requested_at <= ?2",
        params![now_ts, now_ts - END_REQUEST_ESCALATION_THRESHOLD_SECS],
    )?;

    let mut stmt = conn.prepare(
        "SELECT l.id, l.keyholder_id, l.submissive_id, u.display_name, l.end_requested_at,
                l.end_request_reason, l.end_request_escalated_at
         FROM keyholder_submissive_links l
         JOIN users u ON u.id = l.submissive_id
         WHERE l.end_requested_at IS NOT NULL AND l.end_request_escalated_at IS NOT NULL",
    )?;
    let escalated: Vec<PendingEndRequest> = stmt
        .query_map(params![], |row| {
            Ok(PendingEndRequest {
                link_id: row.get(0)?,
                keyholder_id: row.get(1)?,
                submissive_id: row.get(2)?,
                submissive_display_name: row.get(3)?,
                requested_at: row.get(4)?,
                reason: row.get(5)?,
                escalated_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut out = Vec::new();
    for req in escalated {
        if crate::domain::notifications::exists_for_related_entity_since(
            conn,
            "link.end_request_reminder",
            &req.link_id,
            now_ts - END_REQUEST_REMINDER_INTERVAL_SECS,
        )? {
            continue;
        }
        out.push(req);
    }
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum SetStatusError {
    #[error("link not found or not yours")]
    NotFound,
    #[error("that status transition isn't allowed from the link's current status")]
    InvalidTransition,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PATCH /keyholder/submissives/{id}/link` (03-api-design.md §2) —
/// only forward transitions (`active`→`paused`, `active`→`ended`,
/// `paused`→`ended`); there's no way back to `active` here on purpose
/// (a new invite starts a fresh link instead, 02-roles-and-permissions.md
/// §4). Keyholder-scoped, same as every other link mutation.
pub fn set_status(
    conn: &Connection,
    link_id: &str,
    keyholder_id: &str,
    new_status: &str,
) -> Result<(), SetStatusError> {
    let current: Option<String> = conn
        .query_row(
            "SELECT status FROM keyholder_submissive_links WHERE id = ?1 AND keyholder_id = ?2",
            params![link_id, keyholder_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current) = current else {
        return Err(SetStatusError::NotFound);
    };

    let allowed = matches!(
        (current.as_str(), new_status),
        ("active", "paused") | ("active", "ended") | ("paused", "ended")
    );
    if !allowed {
        return Err(SetStatusError::InvalidTransition);
    }

    if new_status == "ended" {
        // Ending a link with a pending end-request clears the request
        // fields as a side effect of the same transaction — the
        // request is now moot, ending already *is* the approval
        // (06-future-extensions.md §2).
        conn.execute(
            "UPDATE keyholder_submissive_links
             SET status = ?1, ended_at = ?2, end_requested_at = NULL,
                 end_requested_by_user_id = NULL, end_request_reason = NULL,
                 end_request_escalated_at = NULL
             WHERE id = ?3",
            params![new_status, crate::auth::session::now(), link_id],
        )?;
    } else {
        conn.execute(
            "UPDATE keyholder_submissive_links SET status = ?1 WHERE id = ?2",
            params![new_status, link_id],
        )?;
    }
    Ok(())
}

pub struct LinkSettings {
    pub self_report_allowed: bool,
    pub catalog_visible_to_submissive: bool,
    /// Points are opt-in per link (01-data-model.md §12,
    /// 11-tasks-and-rewards.md §3) — folded into this same settings
    /// endpoint per 03-api-design.md §10c rather than a separate route.
    pub points_enabled: bool,
}

/// `PATCH /keyholder/submissives/{id}/link/settings` (03-api-design.md
/// §2, §10c). Returns `false` if no such link belongs to this Keyholder.
pub fn set_settings(
    conn: &Connection,
    link_id: &str,
    keyholder_id: &str,
    settings: LinkSettings,
) -> rusqlite::Result<bool> {
    let affected = conn.execute(
        "UPDATE keyholder_submissive_links
         SET self_report_allowed = ?1, catalog_visible_to_submissive = ?2, points_enabled = ?3
         WHERE id = ?4 AND keyholder_id = ?5",
        params![
            settings.self_report_allowed,
            settings.catalog_visible_to_submissive,
            settings.points_enabled,
            link_id,
            keyholder_id
        ],
    )?;
    Ok(affected > 0)
}

/// `(keyholder_id, submissive_id)` for a link — the lookup every
/// notification trigger needs to resolve "who's the other party" from
/// whichever id it already has on hand.
pub fn parties(conn: &Connection, link_id: &str) -> rusqlite::Result<Option<(String, String)>> {
    conn.query_row(
        "SELECT keyholder_id, submissive_id FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// Read side of the settings above — gates the submissive self-report
/// confinement endpoints (03-api-design.md §4) and catalog read access
/// (03-api-design.md §7).
pub fn settings_for_link(conn: &Connection, link_id: &str) -> rusqlite::Result<LinkSettings> {
    conn.query_row(
        "SELECT self_report_allowed, catalog_visible_to_submissive, points_enabled
         FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| {
            Ok(LinkSettings {
                self_report_allowed: row.get(0)?,
                catalog_visible_to_submissive: row.get(1)?,
                points_enabled: row.get(2)?,
            })
        },
    )
}

#[derive(Debug, thiserror::Error)]
pub enum OversightPauseError {
    #[error("no active or paused link")]
    NoLink,
    #[error("oversight is already paused")]
    AlreadyPaused,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST /keyholder/submissives/{id}/oversight-pause`
/// (06-future-extensions.md §13) — a bulk pause one level up from the
/// existing per-session confinement pause: freezes the deadline
/// sweeper's auto-fail pass and new verification-code issuance for
/// this whole link. Cascades into the confinement pause automatically
/// when there's an open, not-already-paused session — "I'm
/// unavailable" is one intent here, not two switches to remember —
/// carrying the same message through so it reads the same wherever
/// it surfaces.
pub fn pause_oversight(
    conn: &Connection,
    keyholder_id: &str,
    submissive_id: &str,
    message: Option<&str>,
) -> Result<(), OversightPauseError> {
    let Some(link_id) = active_or_paused_link_for_keyholder(conn, keyholder_id, submissive_id)?
    else {
        return Err(OversightPauseError::NoLink);
    };
    let affected = conn.execute(
        "UPDATE keyholder_submissive_links
         SET oversight_paused_at = ?1, oversight_pause_message = ?2
         WHERE id = ?3 AND oversight_paused_at IS NULL",
        params![crate::auth::session::now(), message, link_id],
    )?;
    if affected == 0 {
        return Err(OversightPauseError::AlreadyPaused);
    }
    // Best-effort cascade — "nothing open to pause" and "already
    // paused" are both fine no-ops here, not failures of the oversight
    // pause itself.
    let _ = crate::domain::chastity::confinement::pause(conn, submissive_id, message);
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum OversightResumeError {
    #[error("no active or paused link")]
    NoLink,
    #[error("oversight is not currently paused")]
    NotPaused,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug)]
pub struct OversightResumeOutcome {
    pub shifted_assignment_count: i64,
    pub elapsed_seconds: i64,
}

/// `POST /keyholder/submissives/{id}/oversight-resume` — shifts every
/// still-open task/punishment deadline (`status IN ('assigned',
/// 'proof_submitted')`) forward by the elapsed pause duration in one
/// bulk update, mirroring exactly how resuming the confinement pause
/// extends `target_release_at`: without the shift every open deadline
/// would already be in the past the instant the pause lifts, and the
/// sweeper's very next tick would auto-fail all of them at once.
/// Logged as one link-level audit entry summarizing the shift rather
/// than one row per affected assignment.
pub fn resume_oversight(
    conn: &Connection,
    keyholder_id: &str,
    submissive_id: &str,
    resumed_by_user_id: &str,
) -> Result<OversightResumeOutcome, OversightResumeError> {
    let Some(link_id) = active_or_paused_link_for_keyholder(conn, keyholder_id, submissive_id)?
    else {
        return Err(OversightResumeError::NoLink);
    };
    let oversight_paused_at: Option<i64> = conn.query_row(
        "SELECT oversight_paused_at FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| row.get(0),
    )?;
    let Some(paused_at) = oversight_paused_at else {
        return Err(OversightResumeError::NotPaused);
    };
    let elapsed = crate::auth::session::now() - paused_at;

    let shifted = conn.execute(
        "UPDATE assignments SET deadline_at = deadline_at + ?1
         WHERE link_id = ?2 AND status IN ('assigned', 'proof_submitted') AND deadline_at IS NOT NULL",
        params![elapsed, link_id],
    )? as i64;

    conn.execute(
        "UPDATE keyholder_submissive_links
         SET oversight_paused_at = NULL, oversight_pause_message = NULL
         WHERE id = ?1",
        params![link_id],
    )?;

    audit::record(
        conn,
        audit::Entry {
            actor: audit::Actor::User(resumed_by_user_id),
            link_id: Some(&link_id),
            action: "link.oversight_resumed",
            entity_type: "keyholder_submissive_links",
            entity_id: &link_id,
            detail: Some(serde_json::json!({
                "elapsed_seconds": elapsed,
                "shifted_assignment_count": shifted,
            })),
        },
    )?;

    Ok(OversightResumeOutcome {
        shifted_assignment_count: shifted,
        elapsed_seconds: elapsed,
    })
}

/// `PATCH /keyholder/submissives/{id}/oversight-pause-message` — same
/// shape as `confinement::update_pause_message`, one level up.
pub fn update_oversight_pause_message(
    conn: &Connection,
    keyholder_id: &str,
    submissive_id: &str,
    message: Option<&str>,
) -> Result<(), OversightResumeError> {
    let Some(link_id) = active_or_paused_link_for_keyholder(conn, keyholder_id, submissive_id)?
    else {
        return Err(OversightResumeError::NoLink);
    };
    let affected = conn.execute(
        "UPDATE keyholder_submissive_links SET oversight_pause_message = ?1
         WHERE id = ?2 AND oversight_paused_at IS NOT NULL",
        params![message, link_id],
    )?;
    if affected == 0 {
        return Err(OversightResumeError::NotPaused);
    }
    Ok(())
}

const OVERSIGHT_STILL_PAUSED_THRESHOLD_SECS: i64 = 24 * 3600;

pub struct OversightStillPausedReminder {
    pub link_id: String,
    pub keyholder_id: String,
}

/// Mirrors `confinement::run_still_paused_sweep_tick` one level up:
/// every link whose oversight pause has stood for 24h+ without an
/// `oversight.still_paused` reminder in the last 24h, aimed at the
/// Keyholder — the person who paused it is the one most likely to
/// forget it's still on (06-future-extensions.md §13).
pub fn run_oversight_still_paused_sweep_tick(
    conn: &Connection,
) -> rusqlite::Result<Vec<OversightStillPausedReminder>> {
    let now_ts = crate::auth::session::now();
    let candidates: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, keyholder_id FROM keyholder_submissive_links
             WHERE oversight_paused_at IS NOT NULL AND oversight_paused_at <= ?1",
        )?;
        stmt.query_map(
            params![now_ts - OVERSIGHT_STILL_PAUSED_THRESHOLD_SECS],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<rusqlite::Result<_>>()?
    };

    let mut out = Vec::new();
    for (link_id, keyholder_id) in candidates {
        if crate::domain::notifications::exists_for_related_entity_since(
            conn,
            "oversight.still_paused",
            &link_id,
            now_ts - OVERSIGHT_STILL_PAUSED_THRESHOLD_SECS,
        )? {
            continue;
        }
        out.push(OversightStillPausedReminder {
            link_id,
            keyholder_id,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_users(conn: &Connection) -> (String, String) {
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
        (keyholder_id, submissive_id)
    }

    #[test]
    fn creating_a_link_also_creates_its_default_verification_policy() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);

        let link_id = create(&conn, &kh, &sub).unwrap();

        let (frequency_kind, code_ttl): (String, i64) = conn
            .query_row(
                "SELECT frequency_kind, code_ttl_seconds FROM verification_policies WHERE link_id = ?1",
                params![link_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(frequency_kind, "on_demand_only");
        assert_eq!(code_ttl, DEFAULT_CODE_TTL_SECS);
    }

    #[test]
    fn a_submissive_cannot_have_two_active_links() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh1, sub) = seed_users(&conn);
        let kh2 = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'keyholder', 'KH2', 0)",
            params![kh2],
        )
        .unwrap();

        create(&conn, &kh1, &sub).unwrap();
        let second = create(&conn, &kh2, &sub);
        assert!(second.is_err());
    }

    #[test]
    fn force_end_ends_an_active_link_and_is_idempotent_false_after() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        assert!(force_end(&conn, &link_id).unwrap());
        let status: String = conn
            .query_row(
                "SELECT status FROM keyholder_submissive_links WHERE id = ?1",
                params![link_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "ended");

        assert!(!force_end(&conn, &link_id).unwrap());
    }

    #[test]
    fn set_status_allows_forward_transitions_only() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        set_status(&conn, &link_id, &kh, "paused").unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM keyholder_submissive_links WHERE id = ?1",
                params![link_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "paused");

        // Can't go back to active.
        let result = set_status(&conn, &link_id, &kh, "active");
        assert!(matches!(result, Err(SetStatusError::InvalidTransition)));

        set_status(&conn, &link_id, &kh, "ended").unwrap();
        let (status, ended_at): (String, Option<i64>) = conn
            .query_row(
                "SELECT status, ended_at FROM keyholder_submissive_links WHERE id = ?1",
                params![link_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "ended");
        assert!(ended_at.is_some());

        // Nothing is a valid transition out of ended.
        let result = set_status(&conn, &link_id, &kh, "paused");
        assert!(matches!(result, Err(SetStatusError::InvalidTransition)));
    }

    #[test]
    fn set_status_is_scoped_to_the_owning_keyholder() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        let result = set_status(&conn, &link_id, "someone-else", "paused");
        assert!(matches!(result, Err(SetStatusError::NotFound)));
    }

    #[test]
    fn parties_resolves_both_ids_and_none_for_an_unknown_link() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        assert_eq!(parties(&conn, &link_id).unwrap(), Some((kh, sub)));
        assert_eq!(parties(&conn, "no-such-link").unwrap(), None);
    }

    #[test]
    fn settings_default_to_self_report_off_and_catalog_visible() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        let settings = settings_for_link(&conn, &link_id).unwrap();
        assert!(!settings.self_report_allowed);
        assert!(settings.catalog_visible_to_submissive);
    }

    #[test]
    fn set_settings_updates_both_flags_and_is_scoped_to_the_keyholder() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        assert!(
            !set_settings(
                &conn,
                &link_id,
                "someone-else",
                LinkSettings {
                    self_report_allowed: true,
                    catalog_visible_to_submissive: false,
                    points_enabled: false,
                },
            )
            .unwrap()
        );

        assert!(
            set_settings(
                &conn,
                &link_id,
                &kh,
                LinkSettings {
                    self_report_allowed: true,
                    catalog_visible_to_submissive: false,
                    points_enabled: false,
                },
            )
            .unwrap()
        );

        let settings = settings_for_link(&conn, &link_id).unwrap();
        assert!(settings.self_report_allowed);
        assert!(!settings.catalog_visible_to_submissive);
    }

    #[test]
    fn request_end_rejects_a_second_pending_request() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        let requested_link_id = request_end(&conn, &sub, Some("need space")).unwrap();
        assert_eq!(requested_link_id, link_id);

        assert!(matches!(
            request_end(&conn, &sub, None),
            Err(RequestEndError::AlreadyPending)
        ));

        let pending = pending_end_requests_for_keyholder(&conn, &kh).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].reason.as_deref(), Some("need space"));
        assert!(pending[0].escalated_at.is_none());
    }

    #[test]
    fn withdraw_end_request_clears_it_and_allows_a_fresh_one() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        create(&conn, &kh, &sub).unwrap();

        // Nothing to withdraw yet.
        assert_eq!(withdraw_end_request(&conn, &sub).unwrap(), None);

        request_end(&conn, &sub, None).unwrap();
        assert!(withdraw_end_request(&conn, &sub).unwrap().is_some());
        assert!(
            pending_end_requests_for_keyholder(&conn, &kh)
                .unwrap()
                .is_empty()
        );

        // A fresh request works fine after withdrawal.
        request_end(&conn, &sub, Some("still thinking about it")).unwrap();
        assert_eq!(
            pending_end_requests_for_keyholder(&conn, &kh)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn decline_end_request_clears_it_without_ending_the_link_and_is_auditable() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();
        request_end(&conn, &sub, None).unwrap();

        // Someone else's decline attempt fails.
        assert!(matches!(
            decline_end_request(&conn, &link_id, "someone-else"),
            Err(DeclineEndRequestError::NotFound)
        ));

        let declined_submissive_id = decline_end_request(&conn, &link_id, &kh).unwrap();
        assert_eq!(declined_submissive_id, sub);
        assert!(
            pending_end_requests_for_keyholder(&conn, &kh)
                .unwrap()
                .is_empty()
        );
        let status: String = conn
            .query_row(
                "SELECT status FROM keyholder_submissive_links WHERE id = ?1",
                params![link_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "active");

        let audit_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM audit_log WHERE entity_id = ?1 AND action = 'link.end_request_declined'",
                params![link_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audit_count, 1);

        // Declining isn't final — a fresh request can reopen it.
        request_end(&conn, &sub, None).unwrap();
        assert_eq!(
            pending_end_requests_for_keyholder(&conn, &kh)
                .unwrap()
                .len(),
            1
        );

        // A second decline attempt (nothing pending) fails too.
        withdraw_end_request(&conn, &sub).unwrap();
        assert!(matches!(
            decline_end_request(&conn, &link_id, &kh),
            Err(DeclineEndRequestError::NotFound)
        ));
    }

    #[test]
    fn ending_a_link_with_a_pending_request_clears_it_as_a_side_effect() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();
        request_end(&conn, &sub, Some("done")).unwrap();

        set_status(&conn, &link_id, &kh, "ended").unwrap();

        let (requested_at, requested_by): (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT end_requested_at, end_requested_by_user_id FROM keyholder_submissive_links WHERE id = ?1",
                params![link_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(requested_at.is_none());
        assert!(requested_by.is_none());
    }

    #[test]
    fn escalation_sweep_escalates_after_seven_days_and_dedupes_reminders_within_24h() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();
        request_end(&conn, &sub, None).unwrap();

        // Freshly requested — not old enough to escalate yet.
        assert!(
            run_end_request_escalation_sweep_tick(&conn)
                .unwrap()
                .is_empty()
        );

        // Back-date the request past the 7-day threshold.
        conn.execute(
            "UPDATE keyholder_submissive_links SET end_requested_at = ?1 WHERE id = ?2",
            params![
                crate::auth::session::now() - END_REQUEST_ESCALATION_THRESHOLD_SECS - 10,
                link_id
            ],
        )
        .unwrap();

        let escalated = run_end_request_escalation_sweep_tick(&conn).unwrap();
        assert_eq!(escalated.len(), 1);
        assert_eq!(escalated[0].link_id, link_id);
        let pending = pending_end_requests_for_keyholder(&conn, &kh).unwrap();
        assert!(pending[0].escalated_at.is_some());

        // A second tick with no reminder notification recorded yet
        // still reports it (nothing to dedupe against).
        let escalated_again = run_end_request_escalation_sweep_tick(&conn).unwrap();
        assert_eq!(escalated_again.len(), 1);

        // Once a reminder notification is recorded, the sweep goes
        // quiet until the 24h window passes.
        crate::domain::notifications::create(
            &conn,
            crate::domain::notifications::NewNotification {
                user_id: &kh,
                link_id: Some(&link_id),
                notification_type: "link.end_request_reminder",
                title: "Still waiting on your response",
                body: None,
                link_path: None,
                related_entity_type: Some("keyholder_submissive_links"),
                related_entity_id: Some(&link_id),
            },
        )
        .unwrap();
        assert!(
            run_end_request_escalation_sweep_tick(&conn)
                .unwrap()
                .is_empty()
        );
    }

    fn insert_open_task(
        conn: &Connection,
        link_id: &str,
        deadline_at: i64,
        status: &str,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO assignments (id, link_id, kind, title, deadline_at, assigned_at, assigned_via, status)
             VALUES (?1, ?2, 'task', 'Title', ?3, 0, 'session', ?4)",
            params![id, link_id, deadline_at, status],
        )
        .unwrap();
        id
    }

    #[test]
    fn pause_oversight_cascades_into_an_open_confinement_session() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        create(&conn, &kh, &sub).unwrap();
        let device_id = crate::domain::chastity::devices::add(&conn, &sub, "Device", None).unwrap();
        crate::domain::chastity::confinement::start(
            &conn,
            crate::domain::chastity::confinement::StartSession {
                submissive_id: &sub,
                device_id: &device_id,
                started_reason: "voluntary",
                target_release_at: None,
                notes: None,
            },
        )
        .unwrap();

        pause_oversight(&conn, &kh, &sub, Some("traveling")).unwrap();

        let session = crate::domain::chastity::confinement::current(&conn, &sub)
            .unwrap()
            .unwrap();
        assert!(session.clock_paused_at.is_some());

        // A second pause is rejected — already paused.
        let err = pause_oversight(&conn, &kh, &sub, None).unwrap_err();
        assert!(matches!(err, OversightPauseError::AlreadyPaused));
    }

    #[test]
    fn resume_oversight_shifts_open_deadlines_and_clears_the_pause() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        let now = crate::auth::session::now();
        let open_task = insert_open_task(&conn, &link_id, now + 100, "assigned");
        let already_done = insert_open_task(&conn, &link_id, now + 100, "completed");

        // Back-date the pause so there's a real elapsed duration to shift by.
        conn.execute(
            "UPDATE keyholder_submissive_links SET oversight_paused_at = ?1, oversight_pause_message = 'brb' WHERE id = ?2",
            params![now - 500, link_id],
        )
        .unwrap();

        let outcome = resume_oversight(&conn, &kh, &sub, &kh).unwrap();
        assert_eq!(outcome.elapsed_seconds, 500);
        assert_eq!(outcome.shifted_assignment_count, 1);

        let shifted_deadline: i64 = conn
            .query_row(
                "SELECT deadline_at FROM assignments WHERE id = ?1",
                params![open_task],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(shifted_deadline, now + 100 + 500);

        let untouched_deadline: i64 = conn
            .query_row(
                "SELECT deadline_at FROM assignments WHERE id = ?1",
                params![already_done],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(untouched_deadline, now + 100);

        let (paused_at, message): (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT oversight_paused_at, oversight_pause_message FROM keyholder_submissive_links WHERE id = ?1",
                params![link_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(paused_at.is_none());
        assert!(message.is_none());

        // Resuming again is rejected — nothing is paused any more.
        let err = resume_oversight(&conn, &kh, &sub, &kh).unwrap_err();
        assert!(matches!(err, OversightResumeError::NotPaused));
    }

    #[test]
    fn oversight_still_paused_sweep_dedupes_against_an_existing_reminder() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        let conn = pool.get().unwrap();
        let (kh, sub) = seed_users(&conn);
        let link_id = create(&conn, &kh, &sub).unwrap();

        conn.execute(
            "UPDATE keyholder_submissive_links SET oversight_paused_at = ?1 WHERE id = ?2",
            params![
                crate::auth::session::now() - OVERSIGHT_STILL_PAUSED_THRESHOLD_SECS - 10,
                link_id
            ],
        )
        .unwrap();

        let reminders = run_oversight_still_paused_sweep_tick(&conn).unwrap();
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].link_id, link_id);
        assert_eq!(reminders[0].keyholder_id, kh);

        crate::domain::notifications::create(
            &conn,
            crate::domain::notifications::NewNotification {
                user_id: &kh,
                link_id: Some(&link_id),
                notification_type: "oversight.still_paused",
                title: "Still paused",
                body: None,
                link_path: None,
                related_entity_type: Some("keyholder_submissive_links"),
                related_entity_id: Some(&link_id),
            },
        )
        .unwrap();
        assert!(
            run_oversight_still_paused_sweep_tick(&conn)
                .unwrap()
                .is_empty()
        );
    }
}
