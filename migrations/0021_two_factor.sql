-- Optional, opt-in TOTP second factor (01-data-model.md §2,
-- 10-operations.md §2). One row per user; setting up again before
-- confirming replaces the pending row rather than erroring.
CREATE TABLE two_factor_credentials (
    user_id TEXT PRIMARY KEY REFERENCES users(id),
    secret TEXT NOT NULL,
    confirmed_at INTEGER,
    created_at INTEGER NOT NULL
);

-- Generated once, all at once, the moment confirmed_at is set.
CREATE TABLE two_factor_recovery_codes (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    code_hash TEXT NOT NULL,
    used_at INTEGER,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_two_factor_recovery_codes_user_id ON two_factor_recovery_codes(user_id);

-- The gap between "password was correct" and "session issued" when 2FA
-- is enabled — a short-lived, single-purpose, server-tracked
-- intermediate state.
CREATE TABLE two_factor_login_challenges (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0
);
