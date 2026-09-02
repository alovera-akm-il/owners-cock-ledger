-- The reusable catalog (01-data-model.md §6). For kind='task' a template
-- is a complete spec (what satisfies it, how long, what happens either
-- way); for kind IN ('reward','punishment') it's an immediate grant with
-- no deadline/proof of its own.
CREATE TABLE reward_punishment_templates (
    id TEXT PRIMARY KEY,
    keyholder_id TEXT NOT NULL REFERENCES users(id),
    kind TEXT NOT NULL CHECK (kind IN ('reward', 'punishment', 'task')),
    title TEXT NOT NULL,
    description TEXT,
    severity INTEGER,
    active INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    effect_kind TEXT CHECK (effect_kind IN ('grant', 'time_extension', 'time_reduction')),
    completion_type TEXT CHECK (completion_type IN ('acknowledge_only', 'proof_required')),
    proof_media_types TEXT,
    default_deadline_seconds INTEGER,
    time_extension_seconds INTEGER,
    time_reduction_seconds INTEGER,
    -- Self-referencing FKs: SQLite allows a FK to a table mid-CREATE as
    -- long as it exists by the time a row referencing it is written,
    -- which is trivially true here since it's the same table.
    on_success_template_id TEXT REFERENCES reward_punishment_templates(id),
    on_failure_template_id TEXT REFERENCES reward_punishment_templates(id),
    -- points_delta/points_cost are schema-complete now (01-data-model.md
    -- §6) but inert until Phase 6 builds the actual points ledger
    -- (11-tasks-and-rewards.md §3) — no column reads/writes them yet
    -- beyond copying the value through at assignment time.
    points_delta INTEGER,
    points_cost INTEGER
);

CREATE INDEX idx_templates_keyholder_id ON reward_punishment_templates(keyholder_id);
CREATE INDEX idx_templates_kind ON reward_punishment_templates(kind);
