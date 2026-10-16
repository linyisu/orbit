//! Text previews preserve source bytes and bound the content given to a renderer.

const PREVIEW_BYTES: usize = 1_024 * 1_024;

/// Bounded preview content and the original attachment size in bytes.
#[derive(Debug, PartialEq, Eq)]
pub enum FilePreview {
    Text {
        content: String,
        bytes: usize,
        truncated: bool,
    },
    Binary {
        bytes: usize,
    },
}

impl FilePreview {
    /// Decode UTF-8 without replacement characters or interpreting markup. Larger text is cut at
    /// a character boundary; invalid UTF-8 or NUL bytes produce metadata instead of garbled text.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let size = bytes.len();
        let Ok(mut content) = String::from_utf8(bytes) else {
            return Self::Binary { bytes: size };
        };
        if content.contains('\0') {
            return Self::Binary { bytes: size };
        }
        let truncated = size > PREVIEW_BYTES;
        if truncated {
            let mut end = PREVIEW_BYTES;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            content.truncate(end);
        }
        Self::Text {
            content,
            bytes: size,
            truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_literal_and_empty_files_remain_text() {
        for text in ["", "# Generated document\r\n<sample> 数值\t38\n"] {
            assert_eq!(
                FilePreview::from_bytes(text.as_bytes().to_vec()),
                FilePreview::Text {
                    content: text.into(),
                    bytes: text.len(),
                    truncated: false,
                }
            );
        }
    }

    #[test]
    fn binary_data_is_not_replaced_with_lossy_text() {
        for bytes in [vec![255, 254, 1], b"sample\0data".to_vec()] {
            let size = bytes.len();
            assert_eq!(
                FilePreview::from_bytes(bytes),
                FilePreview::Binary { bytes: size }
            );
        }
    }

    #[test]
    fn large_preview_keeps_utf8_boundary_and_original_size() {
        let content = format!("{}界tail", "a".repeat(PREVIEW_BYTES - 1));
        assert_eq!(
            FilePreview::from_bytes(content.as_bytes().to_vec()),
            FilePreview::Text {
                content: "a".repeat(PREVIEW_BYTES - 1),
                bytes: content.len(),
                truncated: true,
            }
        );
    }

    #[test]
    fn local_reads_reject_oversized_and_non_regular_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.txt");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(orbit_model::types::Attachment::MAX_FILE_BYTES as u64 + 1)
            .unwrap();
        assert!(super::super::read_file(&path).is_err());
        assert!(super::super::read_file(directory.path()).is_err());
        assert!(super::super::read_file(&directory.path().join("absent.txt")).is_err());
    }
}
