//! The single call site every domain uses to fire an event from
//! `09-notifications.md`'s trigger matrix: writes the durable
//! `notifications` row (`domain::notifications`) synchronously, then —
//! for push-worthy events — fans a Web Push send out to every active
//! subscription (`domain::push`) in the background, so no caller ever
//! blocks its HTTP response on a third-party push relay. Neither
//! domain module knows about the other; this is where they meet.

use web_push::{
    ContentEncoding, IsahcWebPushClient, SubscriptionInfo, VapidSignatureBuilder, WebPushClient,
    WebPushError, WebPushMessageBuilder,
};

use crate::db::Pool;
use crate::domain::{notifications, push};

pub struct Event<'a> {
    pub user_id: &'a str,
    pub link_id: Option<&'a str>,
    pub notification_type: &'a str,
    pub title: &'a str,
    pub body: Option<&'a str>,
    pub link_path: Option<&'a str>,
    pub related_entity_type: Option<&'a str>,
    pub related_entity_id: Option<&'a str>,
    /// Whether this event is worth an immediate push attempt, per the
    /// "Push?" column of the trigger matrix — `false` means feed-only.
    pub push: bool,
}

/// Writes the notification row and, if `event.push`, spawns the actual
/// network send in the background. Returns once the durable row
/// exists — never waits on push delivery, which can be slow or fail
/// independently of whether the event itself is recorded.
pub async fn notify(pool: &Pool, event: Event<'_>) -> anyhow::Result<notifications::Notification> {
    let owned = OwnedEvent::from(&event);
    let push = event.push;
    let pool2 = pool.clone();
    let notification = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = pool2.get()?;
        Ok(notifications::create(&conn, owned.as_new())?)
    })
    .await??;

    if push {
        let pool3 = pool.clone();
        let n = notification.clone();
        tokio::spawn(async move {
            if let Err(e) = dispatch_push(pool3, n).await {
                tracing::warn!(error = %e, "push dispatch failed");
            }
        });
    }

    Ok(notification)
}

/// Owned copy of `Event`, so the notification-row write can happen on
/// a `spawn_blocking` thread (which requires `'static`) without the
/// caller having to keep its borrowed strings alive that long.
struct OwnedEvent {
    user_id: String,
    link_id: Option<String>,
    notification_type: String,
    title: String,
    body: Option<String>,
    link_path: Option<String>,
    related_entity_type: Option<String>,
    related_entity_id: Option<String>,
}

impl From<&Event<'_>> for OwnedEvent {
    fn from(e: &Event<'_>) -> Self {
        Self {
            user_id: e.user_id.to_string(),
            link_id: e.link_id.map(str::to_string),
            notification_type: e.notification_type.to_string(),
            title: e.title.to_string(),
            body: e.body.map(str::to_string),
            link_path: e.link_path.map(str::to_string),
            related_entity_type: e.related_entity_type.map(str::to_string),
            related_entity_id: e.related_entity_id.map(str::to_string),
        }
    }
}

impl OwnedEvent {
    fn as_new(&self) -> notifications::NewNotification<'_> {
        notifications::NewNotification {
            user_id: &self.user_id,
            link_id: self.link_id.as_deref(),
            notification_type: &self.notification_type,
            title: &self.title,
            body: self.body.as_deref(),
            link_path: self.link_path.as_deref(),
            related_entity_type: self.related_entity_type.as_deref(),
            related_entity_id: self.related_entity_id.as_deref(),
        }
    }
}

/// Same as `notify`, but for background-task call sites (the deadline
/// sweeper) that are already running on a blocking thread rather than
/// in an async API handler — writes the row synchronously on the
/// caller's own connection (one less pool round-trip mid-tick), and
/// spawns the push send the same way.
pub fn notify_sync(
    pool: &Pool,
    conn: &rusqlite::Connection,
    event: Event<'_>,
) -> rusqlite::Result<notifications::Notification> {
    let new = notifications::NewNotification {
        user_id: event.user_id,
        link_id: event.link_id,
        notification_type: event.notification_type,
        title: event.title,
        body: event.body,
        link_path: event.link_path,
        related_entity_type: event.related_entity_type,
        related_entity_id: event.related_entity_id,
    };
    let notification = notifications::create(conn, new)?;

    if event.push {
        let pool = pool.clone();
        let n = notification.clone();
        tokio::spawn(async move {
            if let Err(e) = dispatch_push(pool, n).await {
                tracing::warn!(error = %e, "push dispatch failed");
            }
        });
    }

    Ok(notification)
}

async fn dispatch_push(
    pool: Pool,
    notification: notifications::Notification,
) -> anyhow::Result<()> {
    let pool2 = pool.clone();
    let user_id = notification.user_id.clone();
    let (vapid, subs) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = pool2.get()?;
        let vapid = push::get_or_create_vapid_keys(&conn)?;
        let subs = push::list_active_for_user(&conn, &user_id)?;
        Ok((vapid, subs))
    })
    .await??;

    if subs.is_empty() {
        return Ok(());
    }

    let payload = serde_json::json!({
        "type": notification.notification_type,
        "title": notification.title,
        "body": notification.body,
        "link_path": notification.link_path,
    })
    .to_string();

    let client = IsahcWebPushClient::new()?;

    for sub in &subs {
        let subscription_info = SubscriptionInfo::new(
            sub.endpoint.clone(),
            sub.p256dh_key.clone(),
            sub.auth_key.clone(),
        );

        let result = send_one(
            &client,
            &vapid.private_key_b64,
            &subscription_info,
            &payload,
        )
        .await;

        match result {
            Ok(()) => {
                let pool = pool.clone();
                let sub_id = sub.id.clone();
                let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let conn = pool.get()?;
                    push::touch_last_seen(&conn, &sub_id)?;
                    Ok(())
                })
                .await;
            }
            Err(SendError::Gone) => {
                let pool = pool.clone();
                let endpoint = sub.endpoint.clone();
                let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let conn = pool.get()?;
                    push::remove_by_endpoint(&conn, &endpoint)?;
                    Ok(())
                })
                .await;
            }
            Err(SendError::Other(e)) => {
                tracing::warn!(error = %e, endpoint = %sub.endpoint, "push send failed");
            }
        }
    }

    let pool = pool.clone();
    let id = notification.id.clone();
    let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = pool.get()?;
        notifications::mark_push_dispatched(&conn, &id)?;
        Ok(())
    })
    .await;

    Ok(())
}

enum SendError {
    /// The push service reports the subscription no longer exists
    /// (404/410) — the caller should delete it (09-notifications.md §2
    /// step 4).
    Gone,
    Other(WebPushError),
}

async fn send_one(
    client: &IsahcWebPushClient,
    vapid_private_key_b64: &str,
    subscription_info: &SubscriptionInfo,
    payload: &str,
) -> Result<(), SendError> {
    let sig_builder = VapidSignatureBuilder::from_base64(vapid_private_key_b64, subscription_info)
        .map_err(SendError::Other)?;
    let signature = sig_builder.build().map_err(SendError::Other)?;

    let mut builder = WebPushMessageBuilder::new(subscription_info);
    builder.set_payload(ContentEncoding::Aes128Gcm, payload.as_bytes());
    builder.set_vapid_signature(signature);
    let message = builder.build().map_err(SendError::Other)?;

    // The client's own `send` never times out (crate docs) — a hung
    // TCP connection to a dead push relay shouldn't leak a task
    // forever.
    let send = client.send(message);
    match tokio::time::timeout(std::time::Duration::from_secs(15), send).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(WebPushError::EndpointNotFound(_))) | Ok(Err(WebPushError::EndpointNotValid(_))) => {
            Err(SendError::Gone)
        }
        Ok(Err(e)) => Err(SendError::Other(e)),
        Err(_) => Err(SendError::Other(WebPushError::Unspecified)),
    }
}
