-- Widens the check-in "photo" field type into photo/video (no rename —
-- existing 'photo' rows keep working, the upload endpoint just now also
-- accepts video/mp4 and video/webm), and adds a separate 'audio' field
-- type for voice memos as its own independent attachment slot, same
-- one-column-pair-per-slot shape as photo_attachment_path/photo_mime_type.
CREATE TABLE checkin_template_fields_new (
    id TEXT PRIMARY KEY,
    template_id TEXT NOT NULL REFERENCES checkin_templates(id),
    position INTEGER NOT NULL,
    field_key TEXT NOT NULL,
    label TEXT NOT NULL,
    description TEXT,
    field_type TEXT NOT NULL CHECK (field_type IN ('scale', 'select', 'number', 'text', 'boolean', 'photo', 'audio')),
    config TEXT NOT NULL,
    required INTEGER NOT NULL DEFAULT 0
);

INSERT INTO checkin_template_fields_new SELECT * FROM checkin_template_fields;
DROP TABLE checkin_template_fields;
ALTER TABLE checkin_template_fields_new RENAME TO checkin_template_fields;

CREATE INDEX idx_checkin_template_fields_template_id ON checkin_template_fields(template_id);

ALTER TABLE checkins ADD COLUMN audio_attachment_path TEXT;
ALTER TABLE checkins ADD COLUMN audio_mime_type TEXT;
