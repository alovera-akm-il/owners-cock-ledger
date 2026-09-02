-- Backs account recovery for a locked-out user (01-data-model.md §2).
-- Two issuance paths converge here: `admin reset-password` (always
-- available) and, if the deployer opts into outbound email, self-service
-- via POST /auth/password-reset/request — both redeemed identically.
CREATE TABLE password_reset_tokens (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    token_hash TEXT NOT NULL,
    requested_via TEXT NOT NULL CHECK (requested_via IN ('admin_cli', 'self_service')),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER
);

CREATE INDEX idx_password_reset_tokens_user_id ON password_reset_tokens(user_id);
