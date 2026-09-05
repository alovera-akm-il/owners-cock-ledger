//! Blob directory read/write (07-tech-stack.md §2): private files outside
//! any web-served path, named by a fresh UUID rather than the original
//! filename. Image uploads are decoded and re-encoded, which strips EXIF
//! (GPS, device info) as a side effect and also validates the bytes are a
//! genuine image rather than a mislabeled file
//! (05-security-and-privacy.md §4).

use std::path::Path;

use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("unsupported content type: {0}")]
    UnsupportedContentType(String),
    #[error("uploaded image could not be decoded")]
    InvalidImage,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Re-encoded on ingest to strip EXIF/embedded metadata. Video/audio
/// types are stored as-is — full container metadata stripping (unlike
/// images) needs a real transcoder and isn't built yet
/// (07-tech-stack.md §1); `extension_for` returning `None` for anything
/// else is the defense against an arbitrary upload pretending to be
/// accepted media.
const IMAGE_TYPES: &[&str] = &["image/jpeg", "image/png"];

pub fn extension_for(content_type: &str) -> Option<&'static str> {
    match content_type {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        "audio/webm" => Some("weba"),
        "audio/mp4" => Some("m4a"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some("wav"),
        _ => None,
    }
}

pub struct StoredFile {
    /// A filename within the blob dir, not a full path — callers rejoin
    /// it with the configured blob dir at read time rather than trusting
    /// a stored absolute path.
    pub storage_path: String,
    pub byte_size: i64,
    pub sha256: String,
}

/// Processes and writes one uploaded file into `blob_dir`.
pub fn store(blob_dir: &Path, content_type: &str, bytes: &[u8]) -> Result<StoredFile, StoreError> {
    let Some(ext) = extension_for(content_type) else {
        return Err(StoreError::UnsupportedContentType(content_type.to_string()));
    };

    let processed = if IMAGE_TYPES.contains(&content_type) {
        strip_image_metadata(bytes, content_type)?
    } else {
        bytes.to_vec()
    };

    let sha256 = {
        let digest = Sha256::digest(&processed);
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };

    let filename = format!("{}.{ext}", uuid::Uuid::new_v4());
    std::fs::create_dir_all(blob_dir)?;
    std::fs::write(blob_dir.join(&filename), &processed)?;

    Ok(StoredFile {
        storage_path: filename,
        byte_size: processed.len() as i64,
        sha256,
    })
}

fn strip_image_metadata(bytes: &[u8], content_type: &str) -> Result<Vec<u8>, StoreError> {
    let img = image::load_from_memory(bytes).map_err(|_| StoreError::InvalidImage)?;
    let format = if content_type == "image/png" {
        image::ImageFormat::Png
    } else {
        image::ImageFormat::Jpeg
    };
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), format)
        .map_err(|_| StoreError::InvalidImage)?;
    Ok(out)
}

pub fn read(blob_dir: &Path, storage_path: &str) -> std::io::Result<Vec<u8>> {
    std::fs::read(blob_dir.join(storage_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal valid 1x1 PNG, so tests don't need a real photo fixture.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn stores_a_valid_png_and_reads_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let stored = store(dir.path(), "image/png", TINY_PNG).unwrap();
        assert!(stored.storage_path.ends_with(".png"));
        assert_eq!(stored.sha256.len(), 64);

        let bytes = read(dir.path(), &stored.storage_path).unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn rejects_an_unsupported_content_type() {
        let dir = tempfile::tempdir().unwrap();
        let result = store(dir.path(), "application/pdf", b"not media");
        assert!(matches!(result, Err(StoreError::UnsupportedContentType(_))));
    }

    #[test]
    fn rejects_bytes_that_are_not_actually_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let result = store(dir.path(), "image/png", b"this is not a png");
        assert!(matches!(result, Err(StoreError::InvalidImage)));
    }

    #[test]
    fn video_bytes_are_stored_unmodified() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"pretend this is an mp4 container";
        let stored = store(dir.path(), "video/mp4", payload).unwrap();
        let bytes = read(dir.path(), &stored.storage_path).unwrap();
        assert_eq!(bytes, payload);
    }

    #[test]
    fn mp3_and_wav_are_accepted_and_stored_unmodified() {
        let dir = tempfile::tempdir().unwrap();
        let mp3 = b"pretend this is an mp3 frame";
        let stored = store(dir.path(), "audio/mpeg", mp3).unwrap();
        assert_eq!(read(dir.path(), &stored.storage_path).unwrap(), mp3);
        assert!(stored.storage_path.ends_with(".mp3"));

        let wav = b"pretend this is a wav riff";
        let stored = store(dir.path(), "audio/wav", wav).unwrap();
        assert_eq!(read(dir.path(), &stored.storage_path).unwrap(), wav);
        assert!(stored.storage_path.ends_with(".wav"));
    }

    #[test]
    fn distinct_uploads_get_distinct_filenames() {
        let dir = tempfile::tempdir().unwrap();
        let a = store(dir.path(), "image/png", TINY_PNG).unwrap();
        let b = store(dir.path(), "image/png", TINY_PNG).unwrap();
        assert_ne!(a.storage_path, b.storage_path);
    }
}
