-- Push notifications & in-app feed (01-data-model.md §10,
-- 09-notifications.md). notifications is the durable record every
-- trigger writes to regardless of push outcome; push_subscriptions is
-- the opt-in delivery mechanism layered on top; vapid_keys holds the
-- single server keypair used to sign every push (generated once, on
-- first use, and reused for the life of the deployment).

CREATE TABLE notifications (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    link_id TEXT REFERENCES keyholder_submissive_links(id),
    type TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    link_path TEXT,
    related_entity_type TEXT,
    related_entity_id TEXT,
    created_at INTEGER NOT NULL,
    read_at INTEGER,
    push_dispatched_at INTEGER
);

CREATE INDEX idx_notifications_user_created ON notifications(user_id, created_at DESC);
CREATE INDEX idx_notifications_related_entity ON notifications(type, related_entity_id);

CREATE TABLE push_subscriptions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    endpoint TEXT NOT NULL UNIQUE,
    p256dh_key TEXT NOT NULL,
    auth_key TEXT NOT NULL,
    user_agent TEXT,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER,
    disabled_at INTEGER
);

CREATE INDEX idx_push_subscriptions_user_id ON push_subscriptions(user_id);

-- Singleton row (id always 1) — the server's own VAPID keypair,
-- generated on first use and stored so every restart signs with the
-- same identity (a browser's PushSubscription is bound to the public
-- key it was created with).
CREATE TABLE vapid_keys (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    private_key_b64 TEXT NOT NULL,
    public_key_b64 TEXT NOT NULL
);
