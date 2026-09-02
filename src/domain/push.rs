//! Web Push subscriptions and the server's VAPID identity
//! (01-data-model.md §10, 09-notifications.md §2). Sync storage only —
//! actually sending a push is a network call, which lives in
//! `crate::notify` alongside the async orchestration that wraps this
//! module and `domain::notifications` together.

use jwt_simple::algorithms::ECDSAP256PublicKeyLike;
use jwt_simple::prelude::ES256KeyPair;
use rusqlite::{Connection, OptionalExtension, params};

pub struct VapidKeys {
    /// URL-safe, no-padding base64 of the raw ES256 private key bytes —
    /// the exact shape `web_push::VapidSignatureBuilder::from_base64`
    /// expects.
    pub private_key_b64: String,
    /// URL-safe, no-padding base64 of the uncompressed P-256 public key
    /// point — handed to the browser's `PushManager.subscribe()` as
    /// `applicationServerKey`.
    pub public_key_b64: String,
}

/// Generates the server's VAPID keypair on first use and persists it
/// (a `PushSubscription` is bound to the public key it was created
/// with, so this has to be stable across restarts) — every later call
/// just reads the same singleton row back.
pub fn get_or_create_vapid_keys(conn: &Connection) -> anyhow::Result<VapidKeys> {
    if let Some(existing) = conn
        .query_row(
            "SELECT private_key_b64, public_key_b64 FROM vapid_keys WHERE id = 1",
            [],
            |row| {
                Ok(VapidKeys {
                    private_key_b64: row.get(0)?,
                    public_key_b64: row.get(1)?,
                })
            },
        )
        .optional()?
    {
        return Ok(existing);
    }

    let key_pair = ES256KeyPair::generate();
    let private_key_b64 = base64_url_no_pad_encode(key_pair.to_bytes().as_slice());
    let public_key_b64 = base64_url_no_pad_encode(
        key_pair
            .public_key()
            .public_key()
            .to_bytes_uncompressed()
            .as_slice(),
    );

    // Race-safe: two requests generating a keypair at the same instant
    // both try to insert; whichever loses just re-reads what won.
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO vapid_keys (id, private_key_b64, public_key_b64) VALUES (1, ?1, ?2)",
        params![private_key_b64, public_key_b64],
    )?;
    if inserted == 1 {
        return Ok(VapidKeys {
            private_key_b64,
            public_key_b64,
        });
    }
    conn.query_row(
        "SELECT private_key_b64, public_key_b64 FROM vapid_keys WHERE id = 1",
        [],
        |row| {
            Ok(VapidKeys {
                private_key_b64: row.get(0)?,
                public_key_b64: row.get(1)?,
            })
        },
    )
    .map_err(Into::into)
}

fn base64_url_no_pad_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub struct Subscription {
    pub id: String,
    // Full mirror of the `push_subscriptions` row; `list_for_user`
    // already scopes by this at the SQL level, so nothing reads it
    // back off the struct today.
    #[allow(dead_code)]
    pub user_id: String,
    pub endpoint: String,
    pub p256dh_key: String,
    pub auth_key: String,
    pub user_agent: Option<String>,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
}

const SUBSCRIPTION_COLUMNS: &str =
    "id, user_id, endpoint, p256dh_key, auth_key, user_agent, created_at, last_seen_at";

fn row_to_subscription(row: &rusqlite::Row) -> rusqlite::Result<Subscription> {
    Ok(Subscription {
        id: row.get(0)?,
        user_id: row.get(1)?,
        endpoint: row.get(2)?,
        p256dh_key: row.get(3)?,
        auth_key: row.get(4)?,
        user_agent: row.get(5)?,
        created_at: row.get(6)?,
        last_seen_at: row.get(7)?,
    })
}

/// `POST /notifications/push-subscriptions` — idempotent on `endpoint`
/// (09-notifications.md §2 step 2): re-registering the same endpoint
/// (e.g. the service worker re-subscribing) updates the existing row
/// rather than erroring or duplicating.
pub fn register(
    conn: &Connection,
    user_id: &str,
    endpoint: &str,
    p256dh_key: &str,
    auth_key: &str,
    user_agent: Option<&str>,
) -> rusqlite::Result<String> {
    let now = crate::auth::session::now();
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT id FROM push_subscriptions WHERE endpoint = ?1",
            params![endpoint],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id) = existing_id {
        conn.execute(
            "UPDATE push_subscriptions
             SET user_id = ?1, p256dh_key = ?2, auth_key = ?3, user_agent = ?4,
                 last_seen_at = ?5, disabled_at = NULL
             WHERE id = ?6",
            params![user_id, p256dh_key, auth_key, user_agent, now, id],
        )?;
        return Ok(id);
    }

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO push_subscriptions
            (id, user_id, endpoint, p256dh_key, auth_key, user_agent, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![id, user_id, endpoint, p256dh_key, auth_key, user_agent, now],
    )?;
    Ok(id)
}

pub fn list_for_user(conn: &Connection, user_id: &str) -> rusqlite::Result<Vec<Subscription>> {
    let sql = format!(
        "SELECT {SUBSCRIPTION_COLUMNS} FROM push_subscriptions
         WHERE user_id = ?1 AND disabled_at IS NULL ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(params![user_id], row_to_subscription)?
        .collect()
}

/// Every live subscription for a user — what `crate::notify` fans a
/// push out to.
pub fn list_active_for_user(
    conn: &Connection,
    user_id: &str,
) -> rusqlite::Result<Vec<Subscription>> {
    list_for_user(conn, user_id)
}

/// `DELETE /notifications/push-subscriptions/{id}` — returns `false`
/// if no row matched (wrong id, or not owned by `user_id`), so the API
/// layer can 404.
pub fn delete(conn: &Connection, id: &str, user_id: &str) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "DELETE FROM push_subscriptions WHERE id = ?1 AND user_id = ?2",
        params![id, user_id],
    )?;
    Ok(n > 0)
}

/// Cleanup when the push service reports the endpoint is gone (HTTP
/// 404/410, 09-notifications.md §2 step 4) — deletes outright rather
/// than retrying it forever.
pub fn remove_by_endpoint(conn: &Connection, endpoint: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM push_subscriptions WHERE endpoint = ?1",
        params![endpoint],
    )?;
    Ok(())
}

pub fn touch_last_seen(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE push_subscriptions SET last_seen_at = ?1 WHERE id = ?2",
        params![crate::auth::session::now(), id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pool() -> (tempfile::TempDir, crate::db::Pool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("db.sqlite3")).unwrap();
        (dir, pool)
    }

    fn seed_user(conn: &Connection) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, display_name, created_at)
             VALUES (?1, ?1 || '@example.test', 'hash', 'submissive', 'Sub', 0)",
            params![id],
        )
        .unwrap();
        id
    }

    #[test]
    fn vapid_keys_are_generated_once_and_stable() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();

        let first = get_or_create_vapid_keys(&conn).unwrap();
        let second = get_or_create_vapid_keys(&conn).unwrap();
        assert_eq!(first.private_key_b64, second.private_key_b64);
        assert_eq!(first.public_key_b64, second.public_key_b64);
        assert!(!first.public_key_b64.is_empty());
    }

    #[test]
    fn registering_the_same_endpoint_twice_updates_instead_of_duplicating() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);

        let id1 = register(
            &conn,
            &user,
            "https://push.example/ep1",
            "p256dh",
            "auth",
            None,
        )
        .unwrap();
        let id2 = register(
            &conn,
            &user,
            "https://push.example/ep1",
            "p256dh-new",
            "auth-new",
            Some("Chrome"),
        )
        .unwrap();

        assert_eq!(id1, id2);
        let subs = list_for_user(&conn, &user).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].p256dh_key, "p256dh-new");
    }

    #[test]
    fn delete_is_scoped_to_the_owning_user() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);
        let other = seed_user(&conn);

        let id = register(
            &conn,
            &user,
            "https://push.example/ep2",
            "p256dh",
            "auth",
            None,
        )
        .unwrap();

        assert!(!delete(&conn, &id, &other).unwrap());
        assert!(delete(&conn, &id, &user).unwrap());
        assert_eq!(list_for_user(&conn, &user).unwrap().len(), 0);
    }

    #[test]
    fn remove_by_endpoint_cleans_up_a_dead_subscription() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().unwrap();
        let user = seed_user(&conn);
        register(
            &conn,
            &user,
            "https://push.example/ep3",
            "p256dh",
            "auth",
            None,
        )
        .unwrap();

        remove_by_endpoint(&conn, "https://push.example/ep3").unwrap();
        assert_eq!(list_for_user(&conn, &user).unwrap().len(), 0);
    }
}
