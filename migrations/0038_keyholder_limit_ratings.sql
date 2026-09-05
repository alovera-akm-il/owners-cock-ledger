-- Mirrors submissive_limit_ratings (0029_limits.sql), for a Keyholder
-- rating their own limits against their own catalog — same "no row =
-- not discussed, never coerced to okay" semantics. A Keyholder's
-- catalog-owner id and rating-owner id are the same person here
-- (unlike the submissive case, where a submissive rates their
-- Keyholder's catalog), so this only ever needs one id.
CREATE TABLE keyholder_limit_ratings (
    id TEXT PRIMARY KEY,
    keyholder_id TEXT NOT NULL REFERENCES users(id),
    limit_item_id TEXT NOT NULL REFERENCES limit_items(id),
    rating TEXT NOT NULL CHECK (rating IN ('hard', 'soft', 'okay')),
    notes TEXT,
    updated_at INTEGER NOT NULL,
    UNIQUE (keyholder_id, limit_item_id)
);

CREATE INDEX idx_keyholder_limit_ratings_keyholder_id ON keyholder_limit_ratings(keyholder_id);
