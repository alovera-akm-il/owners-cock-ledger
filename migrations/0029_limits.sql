-- Structured hard/soft limits (01-data-model.md §2 free-text fields
-- stay as-is; this is the checkable layer on top, 06-future-extensions.md
-- §9). limit_items is a reusable catalog — keyholder_id NULL ships as a
-- global default every deployment starts with, non-NULL is a
-- Keyholder's own addition visible only to their own submissives.
-- submissive_limit_ratings is per-submissive-per-item; no row at all is
-- the default "not discussed" state, deliberately never coerced to
-- "okay".

CREATE TABLE limit_items (
    id TEXT PRIMARY KEY,
    keyholder_id TEXT REFERENCES users(id),
    category TEXT NOT NULL,
    label TEXT NOT NULL,
    description TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_limit_items_keyholder_id ON limit_items(keyholder_id);

CREATE TABLE submissive_limit_ratings (
    id TEXT PRIMARY KEY,
    submissive_id TEXT NOT NULL REFERENCES users(id),
    limit_item_id TEXT NOT NULL REFERENCES limit_items(id),
    rating TEXT NOT NULL CHECK (rating IN ('hard', 'soft', 'okay')),
    notes TEXT,
    updated_at INTEGER NOT NULL,
    UNIQUE (submissive_id, limit_item_id)
);

CREATE INDEX idx_submissive_limit_ratings_submissive_id ON submissive_limit_ratings(submissive_id);

-- Global starter vocabulary (06-future-extensions.md §9's "seed list")
-- — a modest handful per category, not an attempt at exhaustiveness. A
-- Keyholder extends it with their own items as needed.
INSERT INTO limit_items (id, keyholder_id, category, label, description, active, created_at) VALUES
    ('seed-impact-paddle', NULL, 'Impact', 'Paddle', NULL, 1, 0),
    ('seed-impact-cane', NULL, 'Impact', 'Cane', NULL, 1, 0),
    ('seed-impact-flogger', NULL, 'Impact', 'Flogger', NULL, 1, 0),
    ('seed-bondage-rope', NULL, 'Bondage & Restraint', 'Rope', NULL, 1, 0),
    ('seed-bondage-cuffs', NULL, 'Bondage & Restraint', 'Cuffs', NULL, 1, 0),
    ('seed-bondage-suspension', NULL, 'Bondage & Restraint', 'Suspension', NULL, 1, 0),
    ('seed-sensation-wax', NULL, 'Sensation', 'Wax play', NULL, 1, 0),
    ('seed-sensation-temperature', NULL, 'Sensation', 'Temperature play', NULL, 1, 0),
    ('seed-sensation-deprivation', NULL, 'Sensation', 'Sensory deprivation', NULL, 1, 0),
    ('seed-chastity-extended', NULL, 'Chastity & Denial', 'Extended lock periods', NULL, 1, 0),
    ('seed-chastity-estim', NULL, 'Chastity & Denial', 'Estim', NULL, 1, 0),
    ('seed-fluids-oral', NULL, 'Fluids', 'Oral', NULL, 1, 0),
    ('seed-fluids-watersports', NULL, 'Fluids', 'Watersports', NULL, 1, 0),
    ('seed-psych-degradation', NULL, 'Psychological', 'Degradation / humiliation language', NULL, 1, 0),
    ('seed-psych-cnc', NULL, 'Psychological', 'Consensual non-consent roleplay', NULL, 1, 0),
    ('seed-medical-needle', NULL, 'Medical', 'Needle play', NULL, 1, 0),
    ('seed-medical-breath', NULL, 'Medical', 'Breath play', NULL, 1, 0),
    ('seed-exhibitionism-public', NULL, 'Exhibitionism', 'Public / outdoor exposure', NULL, 1, 0),
    ('seed-exhibitionism-recording', NULL, 'Exhibitionism', 'Photo / video recording', NULL, 1, 0);
