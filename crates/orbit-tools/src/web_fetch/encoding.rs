//! Decode declared legacy charsets without guessing arbitrary binary payloads.
use super::{ToolError, failed};
use encoding_rs::{Encoding, UTF_8};
use regex::Regex;

pub(super) fn text(body: &[u8], media: &str, html: bool) -> Result<String, ToolError> {
    let charset =
        Regex::new(r#"(?i)charset\s*=\s*["']?([^\s;"'/>]+)"#).expect("constant charset pattern");
    let declared = charset
        .captures(media)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned());
    let meta = if html && declared.is_none() {
        let prefix = String::from_utf8_lossy(&body[..body.len().min(4096)]);
        let tags = Regex::new(r"(?is)<meta\b[^>]*>").expect("constant meta pattern");
        tags.find_iter(&prefix).find_map(|tag| {
            charset
                .captures(tag.as_str())
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_owned())
        })
    } else {
        None
    };
    let encoding = match declared.or(meta) {
        Some(label) => Encoding::for_label(label.as_bytes())
            .ok_or_else(|| failed("unsupported declared page charset"))?,
        None => UTF_8,
    };
    let (decoded, _, errors) = encoding.decode(body);
    if errors {
        return Err(failed(
            "page contains invalid bytes for its declared charset",
        ));
    }
    if decoded.contains('\0') {
        return Err(failed("binary content is not supported"));
    }
    Ok(decoded.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_gbk_header_and_meta_and_shift_jis() {
        let source = "<meta charset=gbk><p>你好世界</p>";
        let (bytes, _, errors) = encoding_rs::GBK.encode(source);
        assert!(!errors);
        assert_eq!(
            text(&bytes, "text/html; charset=gbk", true).unwrap(),
            source
        );
        assert_eq!(text(&bytes, "text/html", true).unwrap(), source);
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("日本語");
        assert_eq!(
            text(&bytes, "text/plain; charset=Shift_JIS", false).unwrap(),
            "日本語"
        );
        assert!(text(&[0xff], "text/plain", false).is_err());
        assert!(text(b"abc", "text/plain; charset=unknown", false).is_err());
    }
}
