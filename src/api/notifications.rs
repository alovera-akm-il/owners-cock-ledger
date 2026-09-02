//! Push notifications & in-app feed (03-api-design.md §13). Available
//! to both roles — a notification's `user_id` is the only scoping that
//! matters here, there's no Keyholder-vs-submissive asymmetry the way
//! most of the rest of the API has.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, INTERNAL_ERROR, iso8601};
use crate::auth::session::CurrentUser;
use crate::db::{self, Pool};
use crate::domain::{notifications, push};

const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");

#[derive(Serialize)]
struct VapidPublicKeyResponse {
    public_key: String,
}

/// `GET /notifications/vapid-public-key` — not a secret (09-notifications.md
/// §1), but only meaningful to an authenticated client, so it's still
/// gated behind a session the same as everything else.
async fn vapid_public_key(
    State(pool): State<Pool>,
    _user: CurrentUser,
) -> Result<Json<VapidPublicKeyResponse>, ApiError> {
    let keys = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = pool.get()?;
        push::get_or_create_vapid_keys(&conn)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(VapidPublicKeyResponse {
        public_key: keys.public_key_b64,
    }))
}

#[derive(Deserialize)]
struct SubscriptionKeysPayload {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
struct RegisterSubscriptionRequest {
    endpoint: String,
    keys: SubscriptionKeysPayload,
    user_agent: Option<String>,
}

#[derive(Serialize)]
struct RegisterSubscriptionResponse {
    id: String,
}

/// `POST /notifications/push-subscriptions` — idempotent on `endpoint`.
async fn register_push_subscription(
    State(pool): State<Pool>,
    user: CurrentUser,
    Json(req): Json<RegisterSubscriptionRequest>,
) -> Result<Json<RegisterSubscriptionResponse>, ApiError> {
    let id = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let conn = pool.get()?;
        Ok(push::register(
            &conn,
            &user.user_id,
            &req.endpoint,
            &req.keys.p256dh,
            &req.keys.auth,
            req.user_agent.as_deref(),
        )?)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(RegisterSubscriptionResponse { id }))
}

#[derive(Serialize)]
struct SubscriptionResponse {
    id: String,
    user_agent: Option<String>,
    created_at: String,
    last_seen_at: Option<String>,
}

impl From<push::Subscription> for SubscriptionResponse {
    fn from(s: push::Subscription) -> Self {
        Self {
            id: s.id,
            user_agent: s.user_agent,
            created_at: iso8601(s.created_at),
            last_seen_at: s.last_seen_at.map(iso8601),
        }
    }
}

/// `GET /notifications/push-subscriptions` — never returns the
/// encryption keys back out.
async fn list_push_subscriptions(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<Json<Vec<SubscriptionResponse>>, ApiError> {
    let subs = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<push::Subscription>> {
        let conn = pool.get()?;
        Ok(push::list_for_user(&conn, &user.user_id)?)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(subs.into_iter().map(Into::into).collect()))
}

/// `DELETE /notifications/push-subscriptions/{id}`.
async fn delete_push_subscription(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let deleted = push::delete(&conn, &id, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        if !deleted {
            return Err(NOT_FOUND);
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

#[derive(Deserialize)]
struct ListNotificationsQuery {
    #[serde(default)]
    unread: bool,
}

#[derive(Serialize)]
struct NotificationResponse {
    id: String,
    #[serde(rename = "type")]
    notification_type: String,
    title: String,
    body: Option<String>,
    link_path: Option<String>,
    created_at: String,
    read_at: Option<String>,
}

impl From<notifications::Notification> for NotificationResponse {
    fn from(n: notifications::Notification) -> Self {
        Self {
            id: n.id,
            notification_type: n.notification_type,
            title: n.title,
            body: n.body,
            link_path: n.link_path,
            created_at: iso8601(n.created_at),
            read_at: n.read_at.map(iso8601),
        }
    }
}

/// `GET /notifications` — own feed, newest first. `?unread=true`
/// narrows to unread only; no cursor pagination, see
/// `domain::notifications::list_for_user`.
async fn list_notifications(
    State(pool): State<Pool>,
    user: CurrentUser,
    Query(q): Query<ListNotificationsQuery>,
) -> Result<Json<Vec<NotificationResponse>>, ApiError> {
    let list = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Vec<notifications::Notification>> {
            let conn = pool.get()?;
            Ok(notifications::list_for_user(
                &conn,
                &user.user_id,
                q.unread,
                100,
            )?)
        },
    )
    .await
    .map_err(|_| INTERNAL_ERROR)?
    .map_err(|_| INTERNAL_ERROR)?;

    Ok(Json(list.into_iter().map(Into::into).collect()))
}

/// `PATCH /notifications/{id}/read`.
async fn mark_notification_read(
    State(pool): State<Pool>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        let found =
            notifications::mark_read(&conn, &id, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        if !found {
            return Err(NOT_FOUND);
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

/// `PATCH /notifications/read-all` — the common "clear the badge" action.
async fn mark_all_notifications_read(
    State(pool): State<Pool>,
    user: CurrentUser,
) -> Result<StatusCode, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<StatusCode, ApiError> {
        let conn = pool.get().map_err(|_| INTERNAL_ERROR)?;
        notifications::mark_all_read(&conn, &user.user_id).map_err(|_| INTERNAL_ERROR)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| INTERNAL_ERROR)?
}

pub fn router() -> Router<db::AppState> {
    Router::new()
        .route("/notifications/vapid-public-key", get(vapid_public_key))
        .route(
            "/notifications/push-subscriptions",
            post(register_push_subscription).get(list_push_subscriptions),
        )
        .route(
            "/notifications/push-subscriptions/{id}",
            delete(delete_push_subscription),
        )
        .route("/notifications", get(list_notifications))
        .route("/notifications/{id}/read", patch(mark_notification_read))
        .route(
            "/notifications/read-all",
            patch(mark_all_notifications_read),
        )
}
