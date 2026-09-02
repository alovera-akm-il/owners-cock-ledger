-- Play sessions (01-data-model.md §15, 14-play-sessions.md) — reusable
-- Keyholder-authored templates (submissive-agnostic, since toys are
-- per-submissive) plus actual instances that cover both live and
-- retrospective logging with one state machine.

CREATE TABLE play_session_templates (
    id TEXT PRIMARY KEY,
    keyholder_id TEXT NOT NULL REFERENCES users(id),
    title TEXT NOT NULL,
    setup_notes TEXT,
    suggested_toy_categories TEXT,
    planned_duration_seconds INTEGER,
    checkin_template_id TEXT REFERENCES checkin_templates(id),
    checkin_interval_seconds INTEGER,
    active INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_play_session_templates_keyholder_id ON play_session_templates(keyholder_id);

CREATE TABLE play_sessions (
    id TEXT PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    template_id TEXT REFERENCES play_session_templates(id),
    title TEXT NOT NULL,
    setup_notes TEXT,
    status TEXT NOT NULL CHECK (status IN ('scheduled', 'in_progress', 'pending_judgement', 'completed', 'cancelled')),
    planned_duration_seconds INTEGER,
    checkin_template_id TEXT REFERENCES checkin_templates(id),
    checkin_interval_seconds INTEGER,
    started_at INTEGER,
    ended_at INTEGER,
    safety_check_ok INTEGER,
    judgement_notes TEXT,
    reward_assignment_id TEXT REFERENCES assignments(id),
    punishment_assignment_id TEXT REFERENCES assignments(id),
    assigned_by_user_id TEXT NOT NULL REFERENCES users(id),
    assigned_at INTEGER NOT NULL
);

CREATE INDEX idx_play_sessions_link_id ON play_sessions(link_id);

CREATE TABLE play_session_toys (
    session_id TEXT NOT NULL REFERENCES play_sessions(id),
    toy_id TEXT NOT NULL REFERENCES toys(id),
    PRIMARY KEY (session_id, toy_id)
);

CREATE TABLE play_session_checkin_schedule (
    id TEXT PRIMARY KEY,
    play_session_id TEXT NOT NULL REFERENCES play_sessions(id),
    sequence_number INTEGER NOT NULL,
    planned_offset_seconds INTEGER NOT NULL,
    checkin_template_id TEXT NOT NULL REFERENCES checkin_templates(id),
    fulfilled_checkin_id TEXT REFERENCES checkins(id)
);

CREATE INDEX idx_play_session_checkin_schedule_session_id ON play_session_checkin_schedule(play_session_id);
