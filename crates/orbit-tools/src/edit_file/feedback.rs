//! Diagnostic candidates only. Exact matching remains the write contract.
use orbit_agent::tool::ToolError;
use serde_json::json;
use sha2::{Digest, Sha256};

const MAX_CANDIDATES: usize = 3;
const MAX_EXCERPT: usize = 800;

pub(super) fn mismatch(before: &str, old: &str, positions: &[usize]) -> ToolError {
    let mut candidates = Vec::new();
    let mut total = positions.len();
    if positions.is_empty() {
        let pattern: Vec<_> = old.lines().take(81).collect();
        // Do not perform an unbounded edit-distance search over a large block.
        if !pattern.is_empty()
            && pattern.len() <= 80
            && pattern.iter().any(|s| !s.trim().is_empty())
        {
            let lines: Vec<_> = before.split_inclusive('\n').collect();
            for (index, window) in lines.windows(pattern.len()).enumerate() {
                if window
                    .iter()
                    .zip(&pattern)
                    .all(|(a, b)| a.trim() == b.trim())
                {
                    total += 1;
                    if candidates.len() < MAX_CANDIDATES {
                        let exact = window.concat();
                        candidates.push(json!({
                            "line":index+1, "end_line":index+pattern.len(),
                            "basis":"same lines after trimming whitespace; verify indentation and LF/CRLF",
                            "current_text":exact.chars().take(MAX_EXCERPT).collect::<String>(),
                            "text_truncated":exact.chars().count() > MAX_EXCERPT,
                        }));
                    }
                }
            }
        }
    } else {
        for &start in positions.iter().take(MAX_CANDIDATES) {
            let line = before[..start].bytes().filter(|b| *b == b'\n').count() + 1;
            let context_start = before[..start].rfind('\n').map_or(0, |i| i + 1);
            let end = start + old.len();
            let context_end = before[end..].find('\n').map_or(before.len(), |i| end + i);
            let exact = &before[context_start..context_end];
            candidates.push(
                json!({"line":line, "byte_offset":start, "basis":"exact match",
                    "current_text":exact.chars().take(MAX_EXCERPT).collect::<String>(),
                    "text_truncated":exact.chars().count()>MAX_EXCERPT,
                }),
            );
        }
    }
    ToolError::ExecutionFailed(json!({
        "error":if positions.is_empty() {"text_not_found"} else {"ambiguous_match"},
        "message":if positions.is_empty() {"old_text was not found. Use the current file text, including its whitespace and line endings."} else {"old_text matches multiple locations. Include surrounding context or explicitly set replace_all."},
        "file_sha256":Sha256::digest(before.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>(),
        "exact_matches":positions.len(), "candidates":candidates,
        "candidates_truncated":total>MAX_CANDIDATES,
        "next_step":"Read the candidate line range if text is truncated, or search/read the current file if no candidate is shown. No edit was applied.",
    }).to_string())
}
