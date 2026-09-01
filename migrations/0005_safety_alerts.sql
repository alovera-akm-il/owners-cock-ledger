CREATE TABLE safety_alerts (
    id TEXT PRIMARY KEY,
    submissive_id TEXT NOT NULL REFERENCES users(id),
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    raised_at INTEGER NOT NULL,
    raised_via TEXT NOT NULL DEFAULT 'submissive' CHECK (raised_via IN ('submissive', 'system')),
    -- No REFERENCES checkins(id): the checkins table doesn't exist until
    -- Phase 6 (13-checkins.md). Enforced at the app layer until then.
    related_checkin_id TEXT,
    message TEXT,
    acknowledged_at INTEGER,
    acknowledged_by_user_id TEXT REFERENCES users(id),
    resolved_at INTEGER
);

CREATE INDEX idx_safety_alerts_link_id ON safety_alerts(link_id);
CREATE INDEX idx_safety_alerts_submissive_id ON safety_alerts(submissive_id);
