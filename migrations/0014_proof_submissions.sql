CREATE TABLE proof_submissions (
    id TEXT PRIMARY KEY,
    submissive_id TEXT NOT NULL REFERENCES users(id),
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    purpose TEXT NOT NULL DEFAULT 'verification' CHECK (
        purpose IN ('verification', 'punishment_completion')
    ),
    verification_code_id TEXT REFERENCES verification_codes(id),
    verification_code_value TEXT,
    -- No REFERENCES assignments(id): that table doesn't exist until
    -- Phase 3. purpose='punishment_completion' isn't reachable until
    -- then either; enforced at the app layer in the meantime.
    assignment_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('photo', 'video', 'note', 'mixed')),
    metadata TEXT,
    submitted_at INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (
        status IN ('pending', 'verified', 'redo', 'failed')
    ),
    reviewed_by_user_id TEXT REFERENCES users(id),
    reviewed_at INTEGER,
    review_notes TEXT,
    reviewed_via TEXT CHECK (reviewed_via IN ('session', 'api_token')),
    redo_of_submission_id TEXT REFERENCES proof_submissions(id)
);

CREATE INDEX idx_proof_submissions_link_id ON proof_submissions(link_id);
CREATE INDEX idx_proof_submissions_submissive_id ON proof_submissions(submissive_id);
CREATE INDEX idx_proof_submissions_status ON proof_submissions(status);
