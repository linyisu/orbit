//! Diagnostic candidates only. Exact matching remains the write contract.
use orbit_agent::tool::ToolError;
use serde_json::json;
use sha2::{Digest, Sha256};

mod candidates;

const MAX_CANDIDATES: usize = 3;
const MAX_EXCERPT_CHARS: usize = 800;

pub(super) fn mismatch(before: &str, old: &str, positions: &[usize]) -> ToolError {
    let mut candidates = Vec::new();
    let mut truncated = positions.len() > MAX_CANDIDATES;
    if positions.is_empty() {
        (candidates, truncated) = candidates::find(before, old);
    } else {
        for &start in positions.iter().take(MAX_CANDIDATES) {
            let line = before[..start]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1;
            let context_start = before[..start]
                .rfind('\n')
                .map_or(0, |i| i + 1);
            let end = start + old.len();
            let context_end = before[end..]
                .find('\n')
                .map_or(before.len(), |i| end + i);
            let exact = &before[context_start..context_end];
            candidates.push(json!({
                "line": line,
                "byte_offset": start,
                "basis": "exact match",
                "current_text": exact
                    .chars()
                    .take(MAX_EXCERPT_CHARS)
                    .collect::<String>(),
                "text_truncated": exact.chars().count() > MAX_EXCERPT_CHARS,
            }));
        }
    }
    ToolError::ExecutionFailed(json!({
        "error": if positions.is_empty() {
            "text_not_found"
        } else {
            "ambiguous_match"
        },
        "message": if positions.is_empty() {
            "old_text was not found. Use the current file text, including its whitespace and line \
                endings."
        } else {
            "old_text matches multiple locations. Include surrounding context or explicitly set \
                replace_all."
        },
        "file_sha256": Sha256::digest(before
            .as_bytes())
            .iter()
            .map(|b|format!("{b:02x}"))
            .collect::<String>(),
        "exact_matches": positions.len(),
        "candidates": candidates,
        "candidates_truncated": truncated,
        "next_step": "Read the candidate line range if text is truncated, or search/read the \
            current file if no candidate is shown. No edit was applied.",
    }).to_string())
}
