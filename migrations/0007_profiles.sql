CREATE TABLE keyholder_profiles (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    bio TEXT,
    contact_info TEXT,
    timezone TEXT,
    hard_limits TEXT,
    soft_limits TEXT
);

CREATE TABLE submissive_profiles (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    bio TEXT,
    safeword TEXT,
    hard_limits TEXT,
    soft_limits TEXT,
    emergency_contact TEXT,
    -- Keyholder-editable only, never returned to the submissive's own
    -- profile fetch (01-data-model.md §2) — enforced at the service layer,
    -- not by this column existing here.
    keyholder_notes TEXT,
    timezone TEXT
);
