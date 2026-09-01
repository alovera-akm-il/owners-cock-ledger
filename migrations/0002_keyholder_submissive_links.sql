CREATE TABLE keyholder_submissive_links (
    id TEXT PRIMARY KEY,
    keyholder_id TEXT NOT NULL REFERENCES users(id),
    submissive_id TEXT NOT NULL REFERENCES users(id),
    status TEXT NOT NULL CHECK (status IN ('active', 'paused', 'ended')),
    started_at INTEGER NOT NULL,
    ended_at INTEGER
);

-- A submissive has at most one active keyholder at a time (01-data-model.md §3).
CREATE UNIQUE INDEX idx_ksl_one_active_per_submissive
    ON keyholder_submissive_links(submissive_id)
    WHERE status = 'active';

CREATE INDEX idx_ksl_keyholder_id ON keyholder_submissive_links(keyholder_id);
