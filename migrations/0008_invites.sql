CREATE TABLE invites (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL,
    created_by_keyholder_id TEXT NOT NULL REFERENCES users(id),
    expires_at INTEGER NOT NULL,
    used_at INTEGER,
    used_by_user_id TEXT REFERENCES users(id)
);

CREATE INDEX idx_invites_created_by ON invites(created_by_keyholder_id);
