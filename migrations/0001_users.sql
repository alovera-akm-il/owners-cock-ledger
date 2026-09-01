CREATE TABLE users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('keyholder', 'submissive')),
    display_name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    disabled_at INTEGER,
    failed_login_count INTEGER NOT NULL DEFAULT 0,
    locked_until INTEGER
);
