//! Points (01-data-model.md §12, 11-tasks-and-rewards.md §3) —
//! opt-in per link, an append-only ledger (`point_transactions`) with
//! a cached running total (`keyholder_submissive_links.points_balance`)
//! kept in sync transactionally on every insert. Every write here goes
//! through `award`, so "why do I have 42 points" always has a full,
//! itemized answer.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::domain::rewards_punishments::assignments;
use crate::domain::rewards_punishments::templates;

pub struct Transaction {
    pub id: String,
    // Every caller already knows which link's ledger it asked for —
    // full mirror of the row for callers that don't.
    #[allow(dead_code)]
    pub link_id: String,
    pub delta: i64,
    pub reason: String,
    pub related_entity_type: Option<String>,
    pub related_entity_id: Option<String>,
    pub notes: Option<String>,
    pub created_at: i64,
}

const TX_COLUMNS: &str =
    "id, link_id, delta, reason, related_entity_type, related_entity_id, notes, created_at";

fn row_to_transaction(row: &rusqlite::Row) -> rusqlite::Result<Transaction> {
    Ok(Transaction {
        id: row.get(0)?,
        link_id: row.get(1)?,
        delta: row.get(2)?,
        reason: row.get(3)?,
        related_entity_type: row.get(4)?,
        related_entity_id: row.get(5)?,
        notes: row.get(6)?,
        created_at: row.get(7)?,
    })
}

pub fn balance(conn: &Connection, link_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT points_balance FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| row.get(0),
    )
}

pub fn points_enabled(conn: &Connection, link_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT points_enabled FROM keyholder_submissive_links WHERE id = ?1",
        params![link_id],
        |row| row.get(0),
    )
}

pub fn list_transactions(conn: &Connection, link_id: &str) -> rusqlite::Result<Vec<Transaction>> {
    let sql = format!(
        "SELECT {TX_COLUMNS} FROM point_transactions WHERE link_id = ?1 ORDER BY created_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![link_id], row_to_transaction)?
        .collect()
}

/// Every points write funnels through here: one ledger row, one
/// transactional balance update. Not exposed outside this module —
/// callers go through `award_if_enabled` (task/verification/check-in
/// sources, silently a no-op when points aren't turned on for this
/// link) or `manual_adjustment`/redemption decision (which require
/// points to be on, since those are explicit points-UI actions).
fn award(
    conn: &Connection,
    link_id: &str,
    delta: i64,
    reason: &str,
    related_entity_type: Option<&str>,
    related_entity_id: Option<&str>,
    notes: Option<&str>,
) -> rusqlite::Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::auth::session::now();
    conn.execute(
        "INSERT INTO point_transactions
            (id, link_id, delta, reason, related_entity_type, related_entity_id, notes, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            id,
            link_id,
            delta,
            reason,
            related_entity_type,
            related_entity_id,
            notes,
            now
        ],
    )?;
    conn.execute(
        "UPDATE keyholder_submissive_links SET points_balance = points_balance + ?1 WHERE id = ?2",
        params![delta, link_id],
    )?;
    Ok(())
}

/// The task/verification/check-in earn-and-spend sources
/// (11-tasks-and-rewards.md §3's table) — a silent no-op when points
/// aren't enabled for this link, so call sites (assignment resolution,
/// the deadline sweeper) don't need their own enabled-check.
pub fn award_if_enabled(
    conn: &Connection,
    link_id: &str,
    delta: i64,
    reason: &str,
    related_entity_type: Option<&str>,
    related_entity_id: Option<&str>,
) -> rusqlite::Result<()> {
    if !points_enabled(conn, link_id)? {
        return Ok(());
    }
    award(
        conn,
        link_id,
        delta,
        reason,
        related_entity_type,
        related_entity_id,
        None,
    )
}

#[derive(Debug, Error)]
pub enum AdjustError {
    #[error("points aren't enabled for this link")]
    NotEnabled,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST /keyholder/submissives/{id}/points/adjust` — the Keyholder's
/// always-available manual override, same "final authority" escape
/// hatch used everywhere else in this app.
pub fn manual_adjustment(
    conn: &Connection,
    link_id: &str,
    delta: i64,
    notes: Option<&str>,
) -> Result<(), AdjustError> {
    if !points_enabled(conn, link_id)? {
        return Err(AdjustError::NotEnabled);
    }
    award(conn, link_id, delta, "manual_adjustment", None, None, notes)?;
    Ok(())
}

pub struct RedemptionRequest {
    pub id: String,
    pub link_id: String,
    pub template_id: String,
    pub points_cost: i64,
    pub status: String,
    pub requested_at: i64,
    pub decided_at: Option<i64>,
    pub decided_by_user_id: Option<String>,
    pub resulting_assignment_id: Option<String>,
}

const REQUEST_COLUMNS: &str = "id, link_id, template_id, points_cost, status, requested_at, \
     decided_at, decided_by_user_id, resulting_assignment_id";

fn row_to_request(row: &rusqlite::Row) -> rusqlite::Result<RedemptionRequest> {
    Ok(RedemptionRequest {
        id: row.get(0)?,
        link_id: row.get(1)?,
        template_id: row.get(2)?,
        points_cost: row.get(3)?,
        status: row.get(4)?,
        requested_at: row.get(5)?,
        decided_at: row.get(6)?,
        decided_by_user_id: row.get(7)?,
        resulting_assignment_id: row.get(8)?,
    })
}

pub fn get_request(conn: &Connection, id: &str) -> rusqlite::Result<Option<RedemptionRequest>> {
    conn.query_row(
        &format!("SELECT {REQUEST_COLUMNS} FROM reward_redemption_requests WHERE id = ?1"),
        params![id],
        row_to_request,
    )
    .optional()
}

/// Across every link in `link_ids`, newest first — the Keyholder's
/// combined redemption-requests queue (03-api-design.md §10c).
pub fn list_requests_for_links(
    conn: &Connection,
    link_ids: &[String],
    pending_only: bool,
) -> rusqlite::Result<Vec<RedemptionRequest>> {
    if link_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = link_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT {REQUEST_COLUMNS} FROM reward_redemption_requests
         WHERE link_id IN ({placeholders}) {}
         ORDER BY requested_at DESC",
        if pending_only {
            "AND status = 'pending'"
        } else {
            ""
        }
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = link_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();
    stmt.query_map(params.as_slice(), row_to_request)?.collect()
}

#[derive(Debug, Error)]
pub enum RequestRedemptionError {
    #[error("points aren't enabled for this link")]
    NotEnabled,
    #[error("template not found, or not redeemable")]
    NotRedeemable,
    #[error("insufficient points balance")]
    InsufficientBalance,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `POST /submissive/rewards/{templateId}/redeem` — the one
/// submissive-initiated row outside their own proof/self-report/toy
/// data (11-tasks-and-rewards.md §3): only ever *requesting* against a
/// balance the Keyholder's own templates and grants built up, and the
/// Keyholder still has final approval.
pub fn request_redemption(
    conn: &Connection,
    link_id: &str,
    template_id: &str,
) -> Result<String, RequestRedemptionError> {
    if !points_enabled(conn, link_id)? {
        return Err(RequestRedemptionError::NotEnabled);
    }
    let template =
        templates::get(conn, template_id)?.ok_or(RequestRedemptionError::NotRedeemable)?;
    let Some(points_cost) = template.points_cost else {
        return Err(RequestRedemptionError::NotRedeemable);
    };
    if template.kind != "reward" || !template.active {
        return Err(RequestRedemptionError::NotRedeemable);
    }
    let current_balance = balance(conn, link_id)?;
    if current_balance < points_cost {
        return Err(RequestRedemptionError::InsufficientBalance);
    }

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO reward_redemption_requests
            (id, link_id, template_id, points_cost, status, requested_at)
         VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
        params![
            id,
            link_id,
            template_id,
            points_cost,
            crate::auth::session::now()
        ],
    )?;
    Ok(id)
}

#[derive(Debug, Error)]
pub enum DecideRedemptionError {
    #[error("request not found, or not yours")]
    NotFound,
    #[error("this request has already been decided")]
    AlreadyDecided,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// `PATCH /keyholder/reward-redemption-requests/{id}` `{decision:
/// "approve"|"deny"}` — approve creates the `assignments` row and
/// deducts the points in one transaction; deny just closes the
/// request, no ledger effect.
pub fn decide_redemption(
    conn: &mut Connection,
    request_id: &str,
    keyholder_id: &str,
    approve: bool,
) -> Result<Option<assignments::Assignment>, DecideRedemptionError> {
    let tx = conn.transaction()?;

    let request: Option<RedemptionRequest> = tx
        .query_row(
            "SELECT r.id, r.link_id, r.template_id, r.points_cost, r.status, r.requested_at,
                    r.decided_at, r.decided_by_user_id, r.resulting_assignment_id
             FROM reward_redemption_requests r
             JOIN keyholder_submissive_links l ON l.id = r.link_id
             WHERE r.id = ?1 AND l.keyholder_id = ?2",
            params![request_id, keyholder_id],
            row_to_request,
        )
        .optional()?;
    let Some(request) = request else {
        return Err(DecideRedemptionError::NotFound);
    };
    if request.status != "pending" {
        return Err(DecideRedemptionError::AlreadyDecided);
    }

    let now = crate::auth::session::now();
    let mut resulting_assignment = None;

    if approve {
        let submissive_id: String = tx.query_row(
            "SELECT submissive_id FROM keyholder_submissive_links WHERE id = ?1",
            params![request.link_id],
            |row| row.get(0),
        )?;
        let a = assignments::create(
            &tx,
            &submissive_id,
            &request.link_id,
            assignments::NewAssignment {
                template_id: Some(&request.template_id),
                require_active_template: false,
                assigned_by_user_id: Some(keyholder_id),
                assigned_via: "session",
                notes: Some("Redeemed with points"),
                ..Default::default()
            },
        )
        .map_err(|e| match e {
            assignments::CreateError::Db(e) => DecideRedemptionError::Db(e),
            _ => DecideRedemptionError::Db(rusqlite::Error::InvalidQuery),
        })?;

        award(
            &tx,
            &request.link_id,
            -request.points_cost,
            "redemption",
            Some("reward_redemption_requests"),
            Some(&request.id),
            None,
        )?;

        tx.execute(
            "UPDATE reward_redemption_requests
             SET status = 'approved', decided_at = ?1, decided_by_user_id = ?2, resulting_assignment_id = ?3
             WHERE id = ?4",
            params![now, keyholder_id, a.id, request.id],
        )?;
        resulting_assignment = Some(a);
    } else {
        tx.execute(
            "UPDATE reward_redemption_requests
             SET status = 'denied', decided_at = ?1, decided_by_user_id = ?2
             WHERE id = ?3",
            params![now, keyholder_id, request.id],
        )?;
    }

    tx.commit()?;
    Ok(resulting_assignment)
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
    fn award_if_enabled_is_a_no_op_when_points_are_off() {
        let (_dir, pool, _kh, _sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();

        award_if_enabled(&conn, &link_id, 5, "task_completed", None, None).unwrap();
        assert_eq!(balance(&conn, &link_id).unwrap(), 0);
        assert_eq!(list_transactions(&conn, &link_id).unwrap().len(), 0);
    }

    #[test]
    fn award_if_enabled_updates_balance_and_ledger_when_on() {
        let (_dir, pool, kh, _sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        crate::domain::links::set_settings(
            &conn,
            &link_id,
            &kh,
            crate::domain::links::LinkSettings {
                self_report_allowed: false,
                catalog_visible_to_submissive: true,
                points_enabled: true,
            },
        )
        .unwrap();

        award_if_enabled(
            &conn,
            &link_id,
            5,
            "task_completed",
            Some("assignments"),
            Some("a1"),
        )
        .unwrap();
        award_if_enabled(&conn, &link_id, -2, "task_failed", None, None).unwrap();

        assert_eq!(balance(&conn, &link_id).unwrap(), 3);
        let txs = list_transactions(&conn, &link_id).unwrap();
        assert_eq!(txs.len(), 2);
        assert!(
            txs.iter()
                .any(|t| t.reason == "task_completed" && t.delta == 5)
        );
        assert!(
            txs.iter()
                .any(|t| t.reason == "task_failed" && t.delta == -2)
        );
    }

    #[test]
    fn manual_adjustment_requires_points_enabled() {
        let (_dir, pool, kh, _sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();

        assert!(matches!(
            manual_adjustment(&conn, &link_id, 10, Some("bonus")),
            Err(AdjustError::NotEnabled)
        ));

        crate::domain::links::set_settings(
            &conn,
            &link_id,
            &kh,
            crate::domain::links::LinkSettings {
                self_report_allowed: false,
                catalog_visible_to_submissive: true,
                points_enabled: true,
            },
        )
        .unwrap();
        manual_adjustment(&conn, &link_id, 10, Some("bonus")).unwrap();
        assert_eq!(balance(&conn, &link_id).unwrap(), 10);
    }

    fn enable_points(conn: &Connection, link_id: &str, kh: &str) {
        crate::domain::links::set_settings(
            conn,
            link_id,
            kh,
            crate::domain::links::LinkSettings {
                self_report_allowed: false,
                catalog_visible_to_submissive: true,
                points_enabled: true,
            },
        )
        .unwrap();
    }

    fn seed_reward_template(conn: &Connection, kh: &str, points_cost: i64) -> String {
        templates::create(
            conn,
            kh,
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
                points_cost: Some(points_cost),
            },
        )
        .unwrap()
    }

    #[test]
    fn request_redemption_rejects_insufficient_balance_and_disabled_points() {
        let (_dir, pool, kh, _sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        let template_id = seed_reward_template(&conn, &kh, 20);

        assert!(matches!(
            request_redemption(&conn, &link_id, &template_id),
            Err(RequestRedemptionError::NotEnabled)
        ));

        enable_points(&conn, &link_id, &kh);
        assert!(matches!(
            request_redemption(&conn, &link_id, &template_id),
            Err(RequestRedemptionError::InsufficientBalance)
        ));

        award_if_enabled(&conn, &link_id, 25, "manual_adjustment", None, None).unwrap();
        let request_id = request_redemption(&conn, &link_id, &template_id).unwrap();
        assert!(get_request(&conn, &request_id).unwrap().is_some());
    }

    #[test]
    fn approving_a_redemption_creates_an_assignment_and_deducts_points() {
        let (_dir, pool, kh, sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        enable_points(&conn, &link_id, &kh);
        award_if_enabled(&conn, &link_id, 30, "manual_adjustment", None, None).unwrap();
        let template_id = seed_reward_template(&conn, &kh, 20);
        let request_id = request_redemption(&conn, &link_id, &template_id).unwrap();

        let mut conn_mut = pool.get().unwrap();
        let assignment = decide_redemption(&mut conn_mut, &request_id, &kh, true)
            .unwrap()
            .expect("approval creates an assignment");
        assert_eq!(assignment.kind, "reward");

        assert_eq!(balance(&conn, &link_id).unwrap(), 10);
        let request = get_request(&conn, &request_id).unwrap().unwrap();
        assert_eq!(request.status, "approved");
        assert_eq!(
            request.resulting_assignment_id.as_deref(),
            Some(assignment.id.as_str())
        );

        // Can't decide twice.
        assert!(matches!(
            decide_redemption(&mut conn_mut, &request_id, &kh, true),
            Err(DecideRedemptionError::AlreadyDecided)
        ));

        let _ = sub; // seeded for realism, not asserted on directly here
    }

    #[test]
    fn denying_a_redemption_leaves_the_balance_untouched() {
        let (_dir, pool, kh, _sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        enable_points(&conn, &link_id, &kh);
        award_if_enabled(&conn, &link_id, 30, "manual_adjustment", None, None).unwrap();
        let template_id = seed_reward_template(&conn, &kh, 20);
        let request_id = request_redemption(&conn, &link_id, &template_id).unwrap();

        let mut conn_mut = pool.get().unwrap();
        let assignment = decide_redemption(&mut conn_mut, &request_id, &kh, false).unwrap();
        assert!(assignment.is_none());
        assert_eq!(balance(&conn, &link_id).unwrap(), 30);
        assert_eq!(
            get_request(&conn, &request_id).unwrap().unwrap().status,
            "denied"
        );
    }

    #[test]
    fn decide_redemption_is_scoped_to_the_owning_keyholder() {
        let (_dir, pool, kh, _sub, link_id) = temp_pool_with_link();
        let conn = pool.get().unwrap();
        enable_points(&conn, &link_id, &kh);
        award_if_enabled(&conn, &link_id, 30, "manual_adjustment", None, None).unwrap();
        let template_id = seed_reward_template(&conn, &kh, 20);
        let request_id = request_redemption(&conn, &link_id, &template_id).unwrap();

        let mut conn_mut = pool.get().unwrap();
        assert!(matches!(
            decide_redemption(&mut conn_mut, &request_id, "someone-else", true),
            Err(DecideRedemptionError::NotFound)
        ));
    }
}
