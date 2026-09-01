CREATE TABLE confinement_sessions (
    id TEXT PRIMARY KEY,
    submissive_id TEXT NOT NULL REFERENCES users(id),
    device_id TEXT NOT NULL REFERENCES chastity_devices(id),
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    target_release_at INTEGER,
    clock_paused_at INTEGER,
    clock_pause_message TEXT,
    started_reason TEXT NOT NULL CHECK (
        started_reason IN ('scheduled', 'punishment', 'voluntary', 'other')
    ),
    ended_reason TEXT CHECK (
        ended_reason IN ('scheduled_release', 'reward', 'emergency', 'keyholder_decision', 'other')
    ),
    ended_by_user_id TEXT REFERENCES users(id),
    notes TEXT
);

-- A submissive has at most one open confinement session at a time
-- (01-data-model.md §4).
CREATE UNIQUE INDEX idx_confinement_sessions_one_open
    ON confinement_sessions(submissive_id)
    WHERE ended_at IS NULL;

CREATE INDEX idx_confinement_sessions_submissive_id ON confinement_sessions(submissive_id);
