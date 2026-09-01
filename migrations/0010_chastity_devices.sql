CREATE TABLE chastity_devices (
    id TEXT PRIMARY KEY,
    submissive_id TEXT NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    description TEXT,
    added_at INTEGER NOT NULL,
    retired_at INTEGER
);

CREATE INDEX idx_chastity_devices_submissive_id ON chastity_devices(submissive_id);
