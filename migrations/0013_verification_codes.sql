CREATE TABLE verification_codes (
    id TEXT PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    code TEXT NOT NULL,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER,
    -- No REFERENCES proof_submissions(id): that table is created by the
    -- next migration, after this one. The FK constraint is added there
    -- isn't possible in SQLite without recreating this table, so it's
    -- enforced at the app layer (the same posture already used for a
    -- couple of forward references elsewhere in this schema).
    consumed_by_submission_id TEXT
);

CREATE INDEX idx_verification_codes_link_id ON verification_codes(link_id);
