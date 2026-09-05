-- Optional photo attachment for a check-in, same shape as the toy-photo
-- columns (storage_path + mime_type, blob stored by UUID filename under
-- BlobDir — the mime type has to be kept alongside since the stored
-- filename's extension isn't meant to be parsed back out at serve time).
ALTER TABLE checkins ADD COLUMN photo_attachment_path TEXT;
ALTER TABLE checkins ADD COLUMN photo_mime_type TEXT;
