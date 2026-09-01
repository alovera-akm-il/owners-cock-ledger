CREATE TABLE confinement_adjustments (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES confinement_sessions(id),
    delta_seconds INTEGER NOT NULL,
    reason TEXT NOT NULL CHECK (
        reason IN ('manual', 'punishment_time_extension', 'clock_pause')
    ),
    -- No REFERENCES assignments(id): that table doesn't exist until
    -- Phase 3 (01-data-model.md §6). Enforced at the app layer until then.
    caused_by_assignment_id TEXT,
    adjusted_by_user_id TEXT REFERENCES users(id),
    adjusted_at INTEGER NOT NULL,
    notes TEXT,
    keyholder_reviewed_at INTEGER
);

CREATE INDEX idx_confinement_adjustments_session_id ON confinement_adjustments(session_id);
