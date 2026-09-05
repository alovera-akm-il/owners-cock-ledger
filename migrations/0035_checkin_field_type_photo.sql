-- Recreates checkin_template_fields (SQLite can't ALTER a CHECK
-- constraint) to add 'photo' as a field_type alongside the original
-- five — a template can now require a photo the same way it can
-- require any other field, validated at check-in creation time.
CREATE TABLE checkin_template_fields_new (
    id TEXT PRIMARY KEY,
    template_id TEXT NOT NULL REFERENCES checkin_templates(id),
    position INTEGER NOT NULL,
    field_key TEXT NOT NULL,
    label TEXT NOT NULL,
    description TEXT,
    field_type TEXT NOT NULL CHECK (field_type IN ('scale', 'select', 'number', 'text', 'boolean', 'photo')),
    config TEXT NOT NULL,
    required INTEGER NOT NULL DEFAULT 0
);

INSERT INTO checkin_template_fields_new SELECT * FROM checkin_template_fields;
DROP TABLE checkin_template_fields;
ALTER TABLE checkin_template_fields_new RENAME TO checkin_template_fields;

CREATE INDEX idx_checkin_template_fields_template_id ON checkin_template_fields(template_id);
