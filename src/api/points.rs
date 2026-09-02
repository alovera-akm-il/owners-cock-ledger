//! Points (03-api-design.md §10c). Balance/ledger reads for both
//! roles; manual adjustment and redemption decisions are
//! Keyholder-only; requesting a redemption is submissive-only.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::{CurrentUser, Role};
use crate::db::{self, Pool};
use crate::domain::{links, points};
use crate::notify;

const FORBIDDEN: ApiError = ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not permitted");
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const CONFLICT: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "conflict",
    "request conflicts with current state",
);
const BAD_REQUEST: ApiError =
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "malformed request");

#[derive(Serialize)]
struct TransactionResponse {
    id: String,
    delta: i64,
    reason: String,
    related_entity_type: Option<String>,
    related_entity_id: Option<String>,
    notes: Option<String>,
    created_at: String,
}

impl From<points::Transaction> for TransactionResponse {
    fn from(t: points::Transaction) -> Self {
        Self {
            id: t.id,
            delta: t.delta,
            reason: t.reason,
            related_entity_type: t.related_entity_type,
            related_entity_id: t.related_entity_id,
            notes: t.notes,
            created_at: iso8601(t.created_at),
        }
    }
}

#[derive(Serialize)]
struct PointsResponse {
    enabled: bool,
    balance: i64,
    transactions: Vec<TransactionResponse>,
}

async fn points_for_keyholder(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
) -> Result<Json<PointsResponse>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        Ok(Json(load_points(&conn, &link_id)?))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

async fn points_own(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<PointsResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        Ok(Json(load_points(&conn, &link_id)?))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

fn load_points(conn: &rusqlite::Connection, link_id: &str) -> Result<PointsResponse, ApiError> {
    let enabled = points::points_enabled(conn, link_id).map_err(|_| INTERNAL_ERROR)?;
    let balance = points::balance(conn, link_id).map_err(|_| INTERNAL_ERROR)?;
    let transactions = points::list_transactions(conn, link_id).map_err(|_| INTERNAL_ERROR)?;
    Ok(PointsResponse {
        enabled,
        balance,
        transactions: transactions.into_iter().map(Into::into).collect(),
    })
}

#[derive(Deserialize)]
struct AdjustRequest {
    delta: i64,
    notes: Option<String>,
}

async fn adjust_points(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(submissive_id): Path<String>,
    Json(req): Json<AdjustRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:invites")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id =
            links::active_or_paused_link_for_keyholder(&conn, &user.user_id, &submissive_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
        match points::manual_adjustment(&conn, &link_id, req.delta, req.notes.as_deref()) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(points::AdjustError::NotEnabled) => Err(CONFLICT),
            Err(points::AdjustError::Db(_)) => Err(INTERNAL_ERROR),
        }
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Serialize)]
struct RedemptionRequestResponse {
    id: String,
    link_id: String,
    template_id: String,
    points_cost: i64,
    status: String,
    requested_at: String,
    decided_at: Option<String>,
    decided_by_user_id: Option<String>,
    resulting_assignment_id: Option<String>,
}

impl From<points::RedemptionRequest> for RedemptionRequestResponse {
    fn from(r: points::RedemptionRequest) -> Self {
        Self {
            id: r.id,
            link_id: r.link_id,
            template_id: r.template_id,
            points_cost: r.points_cost,
            status: r.status,
            requested_at: iso8601(r.requested_at),
            decided_at: r.decided_at.map(iso8601),
            decided_by_user_id: r.decided_by_user_id,
            resulting_assignment_id: r.resulting_assignment_id,
        }
    }
}

async fn redeem(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(template_id): Path<String>,
) -> Result<Json<RedemptionRequestResponse>, ApiError> {
    user.require_role(&[Role::Submissive])
        .map_err(|_| FORBIDDEN)?;
    let pool2 = pool.clone();
    let (request, keyholder_id) = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
        let link_id = links::active_link_for_submissive(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(NOT_FOUND)?;
        let request_id = match points::request_redemption(&conn, &link_id, &template_id) {
            Ok(id) => id,
            Err(points::RequestRedemptionError::NotEnabled) => return Err(CONFLICT),
            Err(points::RequestRedemptionError::NotRedeemable) => return Err(NOT_FOUND),
            Err(points::RequestRedemptionError::InsufficientBalance) => return Err(CONFLICT),
            Err(points::RequestRedemptionError::Db(_)) => return Err(INTERNAL_ERROR),
        };
        let request = points::get_request(&conn, &request_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        let (keyholder_id, _) = links::parties(&conn, &link_id)
            .map_err(|_| INTERNAL_ERROR)?
            .ok_or(INTERNAL_ERROR)?;
        Ok((request, keyholder_id))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: &keyholder_id,
            link_id: None,
            notification_type: "points.redemption_requested",
            title: "A reward redemption is waiting on you",
            body: None,
            link_path: Some("/dashboard"),
            related_entity_type: Some("reward_redemption_requests"),
            related_entity_id: Some(&request.id),
            push: true,
        },
    )
    .await;

    Ok(Json(request.into()))
}

#[derive(Deserialize)]
struct ListRedemptionRequestsQuery {
    #[serde(default)]
    all: bool,
}

async fn list_redemption_requests(
    State(pool): State<Pool>,
    user: CurrentUser,
    axum::extract::Query(q): axum::extract::Query<ListRedemptionRequestsQuery>,
) -> Result<Json<Vec<RedemptionRequestResponse>>, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("read:submissives")
        .map_err(|_| FORBIDDEN)?;
    tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let link_ids = links::active_link_ids_for_keyholder(&conn, &user.user_id)
            .map_err(|_| INTERNAL_ERROR)?;
        let list = points::list_requests_for_links(&conn, &link_ids, !q.all)
            .map_err(|_| INTERNAL_ERROR)?;
        Ok(Json(list.into_iter().map(Into::into).collect()))
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct DecideRequest {
    decision: String,
}

async fn decide_redemption_request(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<DecideRequest>,
) -> Result<StatusCode, ApiError> {
    user.require_role(&[Role::Keyholder])
        .map_err(|_| FORBIDDEN)?;
    user.require_scope("manage:assignments")
        .map_err(|_| FORBIDDEN)?;
    let approve = match req.decision.as_str() {
        "approve" => true,
        "deny" => false,
        _ => return Err(BAD_REQUEST),
    };

    let pool2 = pool.clone();
    let (submissive_id, resulting_assignment) = tokio::task::spawn_blocking(
        move || -> Result<(String, Option<(String, String)>), ApiError> {
            let mut conn = pool2.get().map_err(|_| INTERNAL_ERROR)?;
            let request = points::get_request(&conn, &id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(NOT_FOUND)?;
            let (_, submissive_id) = links::parties(&conn, &request.link_id)
                .map_err(|_| INTERNAL_ERROR)?
                .ok_or(INTERNAL_ERROR)?;
            let assignment = match points::decide_redemption(&mut conn, &id, &user.user_id, approve)
            {
                Ok(a) => a,
                Err(points::DecideRedemptionError::NotFound) => return Err(NOT_FOUND),
                Err(points::DecideRedemptionError::AlreadyDecided) => return Err(CONFLICT),
                Err(points::DecideRedemptionError::Db(_)) => return Err(INTERNAL_ERROR),
            };
            Ok((submissive_id, assignment.map(|a| (a.id, a.title))))
        },
    )
    .await
    .map_err(|_| INTERNAL_ERROR)??;

    let (title, related_entity_id) = match &resulting_assignment {
        Some((id, title)) => (format!("Reward redeemed: {title}"), Some(id.as_str())),
        None => (
            "Your reward redemption request was denied".to_string(),
            None,
        ),
    };
    let _ = notify::notify(
        &pool,
        notify::Event {
            user_id: &submissive_id,
            link_id: None,
            notification_type: "points.redemption_resolved",
            title: &title,
            body: None,
            link_path: Some("/submissive"),
            related_entity_type: related_entity_id.map(|_| "assignments"),
            related_entity_id,
            push: true,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route(
            "/keyholder/submissives/{id}/points",
            get(points_for_keyholder),
        )
        .route("/submissive/points", get(points_own))
        .route(
            "/keyholder/submissives/{id}/points/adjust",
            post(adjust_points),
        )
        .route("/submissive/rewards/{templateId}/redeem", post(redeem))
        .route(
            "/keyholder/reward-redemption-requests",
            get(list_redemption_requests),
        )
        .route(
            "/keyholder/reward-redemption-requests/{id}",
            patch(decide_redemption_request),
        )
}
