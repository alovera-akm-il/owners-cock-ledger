-- confinement_adjustments.reason had no value for a reward-driven
-- time_reduction (only 'punishment_time_extension') even though
-- 08-punishments-and-deadlines.md §6a explicitly needs one — an
-- escalated time_reduction gets the identical "applied automatically,
-- flagged for Keyholder review" treatment as a time_extension, and
-- logging it under a reason literally named "punishment" would be
-- wrong regardless of delta_seconds' sign. Recreated (SQLite can't
-- ALTER a CHECK constraint) with 'reward_time_reduction' added.
CREATE TABLE confinement_adjustments_new (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES confinement_sessions(id),
    delta_seconds INTEGER NOT NULL,
    reason TEXT NOT NULL CHECK (
        reason IN ('manual', 'punishment_time_extension', 'reward_time_reduction', 'clock_pause')
    ),
    caused_by_assignment_id TEXT,
    adjusted_by_user_id TEXT REFERENCES users(id),
    adjusted_at INTEGER NOT NULL,
    notes TEXT,
    keyholder_reviewed_at INTEGER
);

INSERT INTO confinement_adjustments_new SELECT * FROM confinement_adjustments;
DROP TABLE confinement_adjustments;
ALTER TABLE confinement_adjustments_new RENAME TO confinement_adjustments;

CREATE INDEX idx_confinement_adjustments_session_id ON confinement_adjustments(session_id);
