-- Points (opt-in per link, 01-data-model.md §12, 11-tasks-and-rewards.md
-- §3): a running total on top of the existing direct reward/punishment
-- mechanism, and the foundation for submissive-initiated redemption
-- requests — the one deliberate exception to "submissives never
-- self-assign."

ALTER TABLE keyholder_submissive_links ADD COLUMN points_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE keyholder_submissive_links ADD COLUMN points_balance INTEGER NOT NULL DEFAULT 0;

CREATE TABLE point_transactions (
    id TEXT PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    delta INTEGER NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN (
        'task_completed', 'task_failed', 'verification_verified',
        'verification_failed', 'verification_missed', 'checkin_logged',
        'manual_adjustment', 'redemption'
    )),
    related_entity_type TEXT,
    related_entity_id TEXT,
    notes TEXT,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_point_transactions_link_id ON point_transactions(link_id);

CREATE TABLE reward_redemption_requests (
    id TEXT PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    template_id TEXT NOT NULL REFERENCES reward_punishment_templates(id),
    points_cost INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'denied')),
    requested_at INTEGER NOT NULL,
    decided_at INTEGER,
    decided_by_user_id TEXT REFERENCES users(id),
    resulting_assignment_id TEXT REFERENCES assignments(id)
);

CREATE INDEX idx_reward_redemption_requests_link_id ON reward_redemption_requests(link_id);
