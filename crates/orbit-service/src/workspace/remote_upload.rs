//! Connection-owned, bounded upload assembly before the existing durable attachment submission.
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use orbit_protocol::{FileUpload, ImageUpload, remote};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const UPLOAD_LIFETIME: Duration = Duration::from_secs(300);

pub(super) struct Upload {
    session: String,
    name: String,
    bytes: usize,
    sha256: String,
    image: bool,
    data: Vec<u8>,
    created: Instant,
}

#[derive(Default)]
pub(super) struct Uploads(HashMap<String, Upload>);

impl Uploads {
    pub fn begin(
        &mut self,
        session: String,
        id: String,
        name: String,
        bytes: usize,
        sha256: String,
        image: bool,
    ) -> Result<()> {
        self.0
            .retain(|_, upload| upload.created.elapsed() < UPLOAD_LIFETIME);
        ensure!(
            !id.is_empty() && id.len() <= 128 && !self.0.contains_key(&id),
            "invalid upload identity"
        );
        ensure!(
            !name.is_empty() && name.len() <= 255 && !name.contains(['/', '\\', '\0']),
            "invalid attachment name"
        );
        ensure!(
            self.0.len() < 8
                && bytes <= remote::ATTACHMENT_TOTAL_BYTES
                && self
                    .0
                    .values()
                    .map(|u| u.bytes)
                    .sum::<usize>()
                    + bytes
                    <= remote::ATTACHMENT_TOTAL_BYTES,
            "attachment budget exceeded"
        );
        ensure!(
            sha256.len() == 64
                && sha256
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit()),
            "invalid attachment digest"
        );
        self.0.insert(
            id,
            Upload {
                session,
                name,
                bytes,
                sha256,
                image,
                data: Vec::new(),
                created: Instant::now(),
            },
        );
        Ok(())
    }

    pub fn chunk(&mut self, id: &str, offset: usize, data: &str) -> Result<()> {
        ensure!(
            data.len() <= remote::ATTACHMENT_CHUNK_BYTES * 4 / 3 + 4,
            "attachment chunk exceeds budget"
        );
        let upload = self
            .0
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("upload expired; select the file again"))?;
        ensure!(upload.created.elapsed() < UPLOAD_LIFETIME, "upload expired");
        let data = STANDARD.decode(data)?;
        ensure!(
            offset == upload.data.len()
                && data.len() <= remote::ATTACHMENT_CHUNK_BYTES
                && data.len() <= upload.bytes.saturating_sub(offset),
            "invalid attachment offset or length"
        );
        upload.data.extend(data);
        Ok(())
    }

    pub fn finish(
        &mut self,
        session: &str,
        ids: &[String],
    ) -> Result<(Vec<ImageUpload>, Vec<FileUpload>)> {
        ensure!(ids.len() <= 8, "too many attachments");
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            ensure!(seen.insert(id), "duplicate attachment");
            let upload = self
                .0
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("attachment missing"))?;
            ensure!(
                upload.session == session && upload.created.elapsed() < UPLOAD_LIFETIME,
                "attachment session or lifetime changed"
            );
            ensure!(
                upload.bytes == upload.data.len()
                    && Sha256::digest(&upload.data)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                        .eq_ignore_ascii_case(&upload.sha256),
                "attachment is incomplete or damaged"
            );
        }
        let mut images = Vec::new();
        let mut files = Vec::new();
        for id in ids {
            let upload = self
                .0
                .remove(id)
                .expect("validated upload");
            let data = STANDARD.encode(upload.data);
            if upload.image {
                images.push(ImageUpload {
                    name: upload.name,
                    data,
                });
            } else {
                files.push(FileUpload {
                    name: upload.name,
                    data,
                });
            }
        }
        Ok((images, files))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upload_rejects_damage_expiry_and_aggregate_overflow() {
        let mut uploads = Uploads::default();
        let begin = |uploads: &mut Uploads, id: &str, bytes| {
            uploads.begin(
                "session".into(),
                id.into(),
                "example.txt".into(),
                bytes,
                "0".repeat(64),
                false,
            )
        };
        begin(&mut uploads, "damaged", 3).unwrap();
        uploads
            .chunk("damaged", 0, "YWJj")
            .unwrap();
        assert!(
            uploads
                .finish("session", &["damaged".into()])
                .is_err()
        );
        assert!(begin(&mut uploads, "oversized", remote::ATTACHMENT_TOTAL_BYTES).is_err());
        uploads
            .0
            .get_mut("damaged")
            .unwrap()
            .created = Instant::now() - UPLOAD_LIFETIME;
        assert!(uploads.chunk("damaged", 3, "").is_err());
        assert!(
            uploads
                .finish("session", &["damaged".into()])
                .is_err()
        );
        begin(&mut uploads, "replacement", remote::ATTACHMENT_TOTAL_BYTES).unwrap();
        assert!(!uploads.0.contains_key("damaged"));
    }

    #[test]
    fn upload_requires_exact_bytes_digest_and_session() {
        let mut uploads = Uploads::default();
        uploads
            .begin(
                "session".into(),
                "file".into(),
                "example.txt".into(),
                3,
                Sha256::digest(b"abc")
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
                false,
            )
            .unwrap();
        assert!(
            uploads
                .chunk("file", 1, "YWJj")
                .is_err()
        );
        uploads
            .chunk("file", 0, "YWI=")
            .unwrap();
        assert!(
            uploads
                .finish("session", &["file".into()])
                .is_err()
        );
        uploads
            .chunk("file", 2, "Yw==")
            .unwrap();
        assert!(
            uploads
                .finish("other", &["file".into()])
                .is_err()
        );
        let (_, files) = uploads
            .finish("session", &["file".into()])
            .unwrap();
        assert_eq!(STANDARD.decode(&files[0].data).unwrap(), b"abc");
        assert!(
            uploads
                .finish("session", &["file".into()])
                .is_err()
        );
    }
}
