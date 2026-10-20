//! Decode command previews without changing retained bytes or their pagination cursors.

#[cfg(windows)]
use windows_sys::Win32::Globalization::CP_OEMCP;

#[derive(Clone, Copy)]
pub enum OutputEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    #[cfg(windows)]
    WindowsOem,
}

impl OutputEncoding {
    /// Inspect the beginning of a stream. BOM-less Windows Unicode output commonly has
    /// ASCII punctuation or line endings with zero high bytes; ordinary UTF-8 has no such pairs.
    pub fn detect(prefix: &[u8]) -> Self {
        if prefix.starts_with(&[0xff, 0xfe]) {
            return Self::Utf16Le;
        }
        if prefix.starts_with(&[0xfe, 0xff]) {
            return Self::Utf16Be;
        }
        #[cfg(windows)]
        {
            let pairs = prefix.as_chunks::<2>().0.iter();
            if pairs.len() >= 2 {
                let le = pairs
                    .clone()
                    .filter(|pair| pair[1] == 0 && pair[0] != 0)
                    .count();
                let be = pairs
                    .clone()
                    .filter(|pair| pair[0] == 0 && pair[1] != 0)
                    .count();
                if le >= 2 && le * 2 >= pairs.len() {
                    return Self::Utf16Le;
                }
                if be >= 2 && be * 2 >= pairs.len() {
                    return Self::Utf16Be;
                }
            }
            if std::str::from_utf8(prefix).is_err_and(|error| error.error_len().is_some()) {
                return Self::WindowsOem;
            }
        }
        Self::Utf8
    }

    /// Return a complete character boundary, using lookahead when a character exceeds the
    /// requested page size. Offsets and lengths remain raw byte counts.
    pub fn page_len(self, bytes: &[u8], max_bytes: usize) -> usize {
        let desired = bytes.len().min(max_bytes);
        match self {
            Self::Utf16Le | Self::Utf16Be => {
                if bytes.len() < 2 {
                    return bytes.len();
                }
                let mut end = desired - desired % 2;
                if end == 0 && bytes.len() >= 2 {
                    end = 2;
                }
                if end >= 2 && end + 2 <= bytes.len() {
                    let last = self.unit([bytes[end - 2], bytes[end - 1]]);
                    if (0xd800..=0xdbff).contains(&last) {
                        if end == 2 {
                            end += 2;
                        } else {
                            end -= 2;
                        }
                    }
                }
                end
            }
            Self::Utf8 => {
                let valid_end = match std::str::from_utf8(bytes) {
                    Ok(_) => bytes.len(),
                    Err(error) if error.error_len().is_none() => error.valid_up_to(),
                    Err(_) => {
                        #[cfg(windows)]
                        return legacy_page_len(bytes, desired);
                        #[cfg(not(windows))]
                        return desired;
                    }
                };
                if desired >= valid_end {
                    return if valid_end == 0 {
                        desired
                    } else {
                        desired.min(valid_end)
                    };
                }
                let text = std::str::from_utf8(&bytes[..valid_end]).expect("validated prefix");
                let mut end = desired;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                if end == 0 {
                    end = desired;
                    while end < valid_end && !text.is_char_boundary(end) {
                        end += 1;
                    }
                }
                end
            }
            #[cfg(windows)]
            Self::WindowsOem => legacy_page_len(bytes, desired),
        }
    }

    /// Decode a raw slice. Windows legacy output falls back to the system OEM code page
    /// only when it is not UTF-8; this cannot infer arbitrary per-program encoding changes.
    pub fn decode(self, bytes: &[u8]) -> String {
        match self {
            Self::Utf8 => {
                if let Ok(text) = std::str::from_utf8(bytes) {
                    return text.to_owned();
                }
                #[cfg(windows)]
                if let Some(text) = decode_legacy(bytes, CP_OEMCP) {
                    return text;
                }
                String::from_utf8_lossy(bytes).into_owned()
            }
            Self::Utf16Le | Self::Utf16Be => {
                let units = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| self.unit(*pair));
                let mut text: String = char::decode_utf16(units)
                    .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect();
                if !bytes.len().is_multiple_of(2) {
                    text.push(char::REPLACEMENT_CHARACTER);
                }
                text
            }
            #[cfg(windows)]
            Self::WindowsOem => decode_legacy(bytes, CP_OEMCP)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    fn unit(self, pair: [u8; 2]) -> u16 {
        match self {
            Self::Utf16Be => u16::from_be_bytes(pair),
            _ => u16::from_le_bytes(pair),
        }
    }
}

#[cfg(windows)]
fn legacy_page_len(bytes: &[u8], desired: usize) -> usize {
    use windows_sys::Win32::Globalization::{GetOEMCP, IsDBCSLeadByteEx};
    // SAFETY: these functions take scalar code pages and bytes, with no pointer arguments.
    let code_page = unsafe { GetOEMCP() };
    let mut end = 0;
    while end < desired {
        // SAFETY: the code page comes from Windows and the byte is within the supplied slice.
        let width = if unsafe { IsDBCSLeadByteEx(code_page, bytes[end]) } != 0 {
            2
        } else {
            1
        };
        if end + width > desired && end > 0 {
            break;
        }
        end = (end + width).min(bytes.len());
    }
    end
}

#[cfg(windows)]
fn decode_legacy(bytes: &[u8], code_page: u32) -> Option<String> {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;
    let length = i32::try_from(bytes.len()).ok()?;
    let mut units = vec![0u16; bytes.len()];
    // SAFETY: both slices are valid for their supplied lengths and do not overlap.
    let count = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            bytes.as_ptr(),
            length,
            units.as_mut_ptr(),
            length,
        )
    };
    (count > 0).then(|| String::from_utf16_lossy(&units[..count as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_pages_preserve_raw_offsets_and_surrogate_pairs() {
        let text = "目录🦀完成";
        for (encoding, bom) in [
            (OutputEncoding::Utf16Le, [0xff, 0xfe]),
            (OutputEncoding::Utf16Be, [0xfe, 0xff]),
        ] {
            let bytes: Vec<u8> = bom
                .into_iter()
                .chain(
                    text.encode_utf16()
                        .flat_map(|unit| match encoding {
                            OutputEncoding::Utf16Be => unit.to_be_bytes(),
                            _ => unit.to_le_bytes(),
                        }),
                )
                .collect();
            let detected = OutputEncoding::detect(&bytes);
            for page_bytes in 1..=bytes.len() {
                let mut offset = 0;
                let mut decoded = String::new();
                while offset < bytes.len() {
                    let available = &bytes[offset..(offset + page_bytes + 3).min(bytes.len())];
                    let count = detected.page_len(available, page_bytes);
                    assert!(count > 0);
                    decoded.push_str(&detected.decode(&available[..count]));
                    offset += count;
                }
                assert_eq!(offset, bytes.len());
                assert_eq!(
                    decoded
                        .strip_prefix('\u{feff}')
                        .unwrap_or(&decoded),
                    text
                );
            }
        }
    }

    #[test]
    fn utf8_pages_do_not_split_multibyte_characters() {
        let bytes = "alpha你好🦀".as_bytes();
        let mut offset = 0;
        let mut decoded = String::new();
        while offset < bytes.len() {
            let available = &bytes[offset..(offset + 4).min(bytes.len())];
            let count = OutputEncoding::Utf8.page_len(available, 1);
            assert!(count > 0);
            decoded.push_str(&OutputEncoding::Utf8.decode(&available[..count]));
            offset += count;
        }
        assert_eq!(decoded, "alpha你好🦀");
    }

    #[cfg(windows)]
    #[test]
    fn chinese_legacy_output_uses_an_explicit_code_page_without_loss() {
        assert_eq!(
            decode_legacy(&[0xd6, 0xd0, 0xce, 0xc4], 936).unwrap(),
            "中文"
        );
    }
}
