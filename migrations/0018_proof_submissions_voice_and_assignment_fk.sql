-- Recreates proof_submissions (SQLite can't ALTER a CHECK constraint or
-- add a FK to an existing column) to:
--   1. add 'voice' to `kind` — 11-tasks-and-rewards.md §2 treats voice
--      as a first-class proof type alongside photo/video, and a task
--      template's proof_media_types can already be ["voice"].
--   2. give assignment_id a real FK now that assignments exists (Phase 3
--      created it after this table, so it couldn't be added originally).
-- No data migration needed — Phase 2/3 aren't deployed anywhere yet.
CREATE TABLE proof_submissions_new (
    id TEXT PRIMARY KEY,
    submissive_id TEXT NOT NULL REFERENCES users(id),
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    purpose TEXT NOT NULL DEFAULT 'verification' CHECK (
        purpose IN ('verification', 'punishment_completion')
    ),
    verification_code_id TEXT REFERENCES verification_codes(id),
    verification_code_value TEXT,
    assignment_id TEXT REFERENCES assignments(id),
    kind TEXT NOT NULL CHECK (kind IN ('photo', 'video', 'voice', 'note', 'mixed')),
    metadata TEXT,
    submitted_at INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (
        status IN ('pending', 'verified', 'redo', 'failed')
    ),
    reviewed_by_user_id TEXT REFERENCES users(id),
    reviewed_at INTEGER,
    review_notes TEXT,
    reviewed_via TEXT CHECK (reviewed_via IN ('session', 'api_token')),
    redo_of_submission_id TEXT REFERENCES proof_submissions_new(id)
);

INSERT INTO proof_submissions_new SELECT * FROM proof_submissions;
DROP TABLE proof_submissions;
ALTER TABLE proof_submissions_new RENAME TO proof_submissions;

CREATE INDEX idx_proof_submissions_link_id ON proof_submissions(link_id);
CREATE INDEX idx_proof_submissions_submissive_id ON proof_submissions(submissive_id);
CREATE INDEX idx_proof_submissions_status ON proof_submissions(status);
