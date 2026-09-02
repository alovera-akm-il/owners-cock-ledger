-- Toy catalog (01-data-model.md §13, 12-toy-catalog.md) — per-submissive,
-- like chastity_devices, not a Keyholder-reusable template. Either role
-- may add; only a Keyholder can retire (soft-delete) — a submissive can
-- only request removal (retirement_requested_at).

CREATE TABLE toys (
    id TEXT PRIMARY KEY,
    submissive_id TEXT NOT NULL REFERENCES users(id),
    added_by_user_id TEXT NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    category TEXT,
    material TEXT,
    brand TEXT,
    size_notes TEXT,
    color TEXT,
    compatible_device_id TEXT REFERENCES chastity_devices(id),
    storage_location TEXT,
    care_instructions TEXT,
    usage_notes TEXT,
    tags TEXT,
    photo_attachment_path TEXT,
    acquired_at INTEGER,
    retirement_requested_at INTEGER,
    retired_at INTEGER,
    retired_by_user_id TEXT REFERENCES users(id)
);

CREATE INDEX idx_toys_submissive_id ON toys(submissive_id);
