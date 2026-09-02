-- Repeating tasks (06-future-extensions.md §14): a rule that
-- periodically spawns an ordinary `assignments` row from an existing
-- kind='task' template. Not a new task state machine — every spawned
-- assignment is a normal one except for the dedicated
-- spawned_by_recurring_task_rule_id column, following the exact
-- precedent triggered_by_play_session_id set.

CREATE TABLE recurring_task_rules (
    id TEXT PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    template_id TEXT NOT NULL REFERENCES reward_punishment_templates(id),
    recurrence_kind TEXT NOT NULL CHECK (recurrence_kind IN ('interval_hours', 'daily', 'weekly_days')),
    recurrence_value TEXT NOT NULL,
    allow_overlap INTEGER NOT NULL DEFAULT 0,
    active INTEGER NOT NULL DEFAULT 1,
    next_due_at INTEGER NOT NULL,
    created_by_user_id TEXT NOT NULL REFERENCES users(id),
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_recurring_task_rules_link_id ON recurring_task_rules(link_id);

ALTER TABLE assignments ADD COLUMN spawned_by_recurring_task_rule_id TEXT;
