-- Schema only in Phase 1: a default row is created alongside every new
-- link (01-data-model.md §5) so there's never an undefined window before
-- a Keyholder configures a real schedule. The actual verification
-- workflow (code issuance, proof review) is Phase 2
-- (04-verification-workflow.md).
CREATE TABLE verification_policies (
    id TEXT PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES keyholder_submissive_links(id),
    frequency_kind TEXT NOT NULL CHECK (
        frequency_kind IN (
            'interval_hours',
            'fixed_times_daily',
            'random_within_window',
            'on_demand_only'
        )
    ),
    frequency_value TEXT NOT NULL,
    code_ttl_seconds INTEGER NOT NULL,
    grace_period_seconds INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX idx_verification_policies_link_id ON verification_policies(link_id);
