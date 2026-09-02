-- An actual instance of a reward, punishment, or task given to a
-- specific submissive (01-data-model.md §6).
CREATE TABLE assignments (
    id TEXT PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    template_id TEXT REFERENCES reward_punishment_templates(id),
    kind TEXT NOT NULL CHECK (kind IN ('reward', 'punishment', 'task')),
    title TEXT NOT NULL,
    description TEXT,
    effect_kind TEXT CHECK (effect_kind IN ('grant', 'time_extension', 'time_reduction')),
    completion_type TEXT CHECK (completion_type IN ('acknowledge_only', 'proof_required')),
    proof_media_types TEXT,
    deadline_at INTEGER,
    time_extension_seconds INTEGER,
    time_reduction_seconds INTEGER,
    -- No REFERENCES proof_submissions(id): that table is recreated right
    -- after this one to point back via assignment_id, and SQLite can't
    -- forward-reference a table that doesn't exist yet at CREATE TABLE
    -- time in a way it can later enforce — the relationship is
    -- symmetric (each points at the other) so one side is necessarily
    -- unconstrained; enforced at the app layer.
    proof_submission_id TEXT,
    on_success_template_id TEXT REFERENCES reward_punishment_templates(id),
    on_failure_template_id TEXT REFERENCES reward_punishment_templates(id),
    escalated_from_assignment_id TEXT REFERENCES assignments(id),
    triggered_by_submission_id TEXT REFERENCES proof_submissions(id),
    -- No REFERENCES play_sessions(id): that table doesn't exist until
    -- Phase 6 (14-play-sessions.md). Enforced at the app layer until then.
    triggered_by_play_session_id TEXT,
    points_delta INTEGER,
    assigned_at INTEGER NOT NULL,
    assigned_by_user_id TEXT REFERENCES users(id),
    assigned_via TEXT NOT NULL CHECK (assigned_via IN ('session', 'api_token', 'system')),
    status TEXT NOT NULL CHECK (
        status IN ('assigned', 'acknowledged', 'proof_submitted', 'completed', 'failed', 'revoked', 'applied')
    ),
    status_updated_at INTEGER,
    notes TEXT
);

CREATE INDEX idx_assignments_link_id ON assignments(link_id);
CREATE INDEX idx_assignments_status ON assignments(status);
CREATE INDEX idx_assignments_deadline_at ON assignments(deadline_at);
CREATE INDEX idx_assignments_escalated_from ON assignments(escalated_from_assignment_id);
