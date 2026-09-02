-- Check-ins (01-data-model.md §14, 13-checkins.md) — Keyholder-authored
-- templates with configurable custom fields, plus the always-present
-- color signal (green/yellow/red) that's schema-level rather than just
-- another field, so it means the same thing on every template.

CREATE TABLE checkin_templates (
    id TEXT PRIMARY KEY,
    keyholder_id TEXT NOT NULL REFERENCES users(id),
    title TEXT NOT NULL,
    description TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    auto_escalate_on_red INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_checkin_templates_keyholder_id ON checkin_templates(keyholder_id);

CREATE TABLE checkin_template_fields (
    id TEXT PRIMARY KEY,
    template_id TEXT NOT NULL REFERENCES checkin_templates(id),
    position INTEGER NOT NULL,
    field_key TEXT NOT NULL,
    label TEXT NOT NULL,
    description TEXT,
    field_type TEXT NOT NULL CHECK (field_type IN ('scale', 'select', 'number', 'text', 'boolean')),
    config TEXT NOT NULL,
    required INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_checkin_template_fields_template_id ON checkin_template_fields(template_id);

CREATE TABLE checkins (
    id TEXT PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    template_id TEXT NOT NULL REFERENCES checkin_templates(id),
    color TEXT NOT NULL CHECK (color IN ('green', 'yellow', 'red')),
    field_values TEXT NOT NULL,
    related_confinement_session_id TEXT REFERENCES confinement_sessions(id),
    related_assignment_id TEXT REFERENCES assignments(id),
    -- No REFERENCES play_sessions(id): that table doesn't exist until
    -- Phase 6's play-sessions step (14-play-sessions.md). Enforced at
    -- the app layer until then, same pattern already used for
    -- safety_alerts.related_checkin_id before checkins itself existed.
    related_play_session_id TEXT,
    created_by_user_id TEXT NOT NULL REFERENCES users(id),
    updated_by_user_id TEXT REFERENCES users(id),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_checkins_link_id ON checkins(link_id);
CREATE INDEX idx_checkins_related_assignment_id ON checkins(related_assignment_id);
CREATE INDEX idx_checkins_related_confinement_session_id ON checkins(related_confinement_session_id);
