-- Keyholder automation tokens (01-data-model.md §9). Submissive-issued
-- tokens don't exist in v1 — enforced at the service layer, not by a
-- schema constraint (the FK target is users(id) generally, since a
-- CHECK can't join to another table's column).
CREATE TABLE api_tokens (
    id TEXT PRIMARY KEY,
    keyholder_id TEXT NOT NULL REFERENCES users(id),
    label TEXT NOT NULL,
    token_prefix TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    scopes TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    last_used_at INTEGER,
    revoked_at INTEGER
);

CREATE INDEX idx_api_tokens_keyholder_id ON api_tokens(keyholder_id);
