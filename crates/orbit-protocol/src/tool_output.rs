//! Read-only paging of retained evidence, independent of model context budgets.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Command { output_id: String, stream: Stream },
    Web { snapshot_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub content: String,
    /// Opaque byte offset into the same retained source; absent at EOF.
    pub next_offset: Option<u64>,
    /// Byte position after this page, including EOF. Used to resume a growing log.
    /// Older peers omit it; viewers must reread rather than infer offsets from decoded text.
    #[serde(default)]
    pub read_offset: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_eof_pages_do_not_invent_a_raw_byte_cursor() {
        let page: Page = serde_json::from_value(serde_json::json!({
            "content": "decoded \u{fffd}\n", "next_offset": null,
        }))
        .unwrap();
        assert!(page.next_offset.is_none());
        assert!(page.read_offset.is_none());
    }
}
