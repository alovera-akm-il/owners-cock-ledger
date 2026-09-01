CREATE TABLE proof_attachments (
    id TEXT PRIMARY KEY,
    submission_id TEXT NOT NULL REFERENCES proof_submissions(id),
    storage_path TEXT NOT NULL,
    original_filename TEXT,
    mime_type TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    uploaded_at INTEGER NOT NULL
);

CREATE INDEX idx_proof_attachments_submission_id ON proof_attachments(submission_id);
