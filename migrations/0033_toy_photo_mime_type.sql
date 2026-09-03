-- Closes the toy-photo gap identified in the mockup audit
-- (docs/16-mockup-implementation-gaps.md item 2): `photo_attachment_path`
-- already existed but nothing ever populated it — no upload route, no
-- UI. Serving a stored photo back needs its content type (the blob
-- store keeps files by UUID filename with an extension, but the API
-- response shouldn't expose or parse that path), so this adds the one
-- column the upload endpoint needs that create/edit never carried.
ALTER TABLE toys ADD COLUMN photo_mime_type TEXT;
