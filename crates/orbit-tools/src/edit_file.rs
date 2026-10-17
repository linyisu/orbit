use orbit_agent::tool::{Tool, ToolError, ToolOutput};
use orbit_runtime::filesystem::Workspace;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{approval::ApprovalSender, write_file::WriteFile};

mod feedback;

const MAX_EDIT_BYTES: usize = 1_024 * 1_024;
const MAX_EDITS: usize = 32;
const MAX_DIFF_BYTES: usize = 16 * 1_024;
const DIFF_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Arguments {
    /// Workspace-relative path or an explicitly granted outside path.
    path: String,
    /// Exact text to replace for a single edit. Use edits for a batch instead.
    #[serde(default)]
    old_text: Option<String>,
    #[serde(default)]
    new_text: Option<String>,
    #[serde(default)]
    replace_all: bool,
    /// Ordered replacements applied in memory, then approved and committed once.
    #[serde(default)]
    #[schemars(length(min = 1, max = MAX_EDITS))]
    edits: Option<Vec<Replacement>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Replacement {
    #[schemars(length(min = 1, max = MAX_EDIT_BYTES))]
    old_text: String,
    #[schemars(length(max = MAX_EDIT_BYTES))]
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}

pub struct EditFile {
    writer: WriteFile,
}

impl EditFile {
    pub fn new(workspace: Workspace, approvals: ApprovalSender) -> Self {
        Self {
            writer: WriteFile::new(workspace, approvals),
        }
    }
}

impl Tool for EditFile {
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::WRITE
    }

    /// Edit an existing UTF-8 file after reading the relevant region. Supply old_text/new_text
    /// for one replacement or edits for an ordered batch. Every old_text must match exactly
    /// once, including line endings, unless its replace_all is true. Empty new_text deletes.
    /// All replacements are validated in memory before one approval and atomic commit;
    /// a failed replacement writes nothing. Concurrent changes reject the commit. A mismatch
    /// includes candidate locations for correction. Files and individual replacement values
    /// are bounded to 1 MiB; a batch accepts at most 32 edits and 2 MiB of replacement text.
    /// Never retry a denied edit without a new user request.
    #[orbit_agent::tool(name = "EditFile")]
    async fn execute(
        &self,
        args: Arguments,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        if args.path.trim().is_empty() {
            return Err(invalid("path must be nonempty"));
        }
        let edits = match (args.old_text, args.new_text, args.edits) {
            (Some(old_text), Some(new_text), None) => vec![Replacement {
                old_text,
                new_text,
                replace_all: args.replace_all,
            }],
            (None, None, Some(edits))
                if !args.replace_all && !edits.is_empty() && edits.len() <= MAX_EDITS =>
            {
                edits
            }
            _ => {
                return Err(invalid(
                    "supply old_text and new_text, or 1..32 edits; do not mix the forms",
                ));
            }
        };
        let mut input_bytes = 0usize;
        for edit in &edits {
            input_bytes = input_bytes
                .saturating_add(edit.old_text.len())
                .saturating_add(edit.new_text.len());
            if edit.old_text.is_empty()
                || edit.old_text.contains('\0')
                || edit.new_text.contains('\0')
                || edit.old_text.len() > MAX_EDIT_BYTES
                || edit.new_text.len() > MAX_EDIT_BYTES
                || input_bytes > 2 * MAX_EDIT_BYTES
            {
                return Err(invalid(
                    "replacement text must contain no NUL; old_text must be nonempty; \
                        each value is limited to 1 MiB and the batch to 2 MiB",
                ));
            }
        }
        let edits_applied = edits.len();
        let mut output = self
            .writer
            .edit(
                args.path,
                move |before| {
                    let mut after = before.to_owned();
                    for (index, edit) in edits.iter().enumerate() {
                        after = replace(&after, edit).map_err(|error| {
                            if edits.len() > 1 {
                                ToolError::ExecutionFailed(format!(
                                    "batch edit {} failed; no changes were written. \
                                        Candidates below describe the intermediate buffer, \
                                        not the file on disk: {error}",
                                    index + 1
                                ))
                            } else {
                                error
                            }
                        })?;
                    }
                    Ok(after)
                },
                cancellation,
            )
            .await?;
        if let Some(change) = output.changes.first() {
            let diff = similar::TextDiff::configure()
                .timeout(DIFF_TIMEOUT)
                .diff_lines(
                    change
                        .before
                        .as_deref()
                        .unwrap_or_default(),
                    &change.after,
                );
            let mut text = diff
                .unified_diff()
                .context_radius(3)
                .to_string();
            let (inserted, deleted) =
                diff.iter_all_changes()
                    .fold((0usize, 0usize), |counts, change| match change.tag() {
                        similar::ChangeTag::Insert => (counts.0 + 1, counts.1),
                        similar::ChangeTag::Delete => (counts.0, counts.1 + 1),
                        similar::ChangeTag::Equal => counts,
                    });
            let truncated = text.len() > MAX_DIFF_BYTES;
            if truncated {
                let mut end = MAX_DIFF_BYTES;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
            }
            if let Ok(serde_json::Value::Object(mut receipt)) =
                serde_json::from_str(&output.content)
            {
                receipt.insert("diff".into(), text.into());
                receipt.insert("diff_truncated".into(), truncated.into());
                receipt.insert("edits_applied".into(), edits_applied.into());
                receipt.insert("inserted_lines".into(), inserted.into());
                receipt.insert("deleted_lines".into(), deleted.into());
                output.content = serde_json::Value::Object(receipt).to_string();
            }
        }
        Ok(output)
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "EditFile".into(),
        message: message.into(),
    }
}

fn replace(before: &str, edit: &Replacement) -> Result<String, ToolError> {
    // Count overlapping candidates too: `aa` in `aaa` is ambiguous.
    let positions: Vec<_> = before
        .char_indices()
        .filter(|(index, _)| before[*index..].starts_with(&edit.old_text))
        .map(|(index, _)| index)
        .collect();
    let count = positions.len();

    if count == 0 {
        return Err(feedback::mismatch(before, &edit.old_text, &positions));
    }
    if count != 1 && !edit.replace_all {
        return Err(feedback::mismatch(before, &edit.old_text, &positions));
    }

    // replace_all uses left-to-right, non-overlapping replacement.
    let count = before.matches(&edit.old_text).count();
    let new_len = before.len() - count * edit.old_text.len();

    if edit
        .new_text
        .len()
        .checked_mul(count)
        .and_then(|n| new_len.checked_add(n))
        .is_none_or(|n| n > MAX_EDIT_BYTES)
    {
        return Err(ToolError::ExecutionFailed(
            "edited file would exceed 1 MiB".into(),
        ));
    }

    Ok(before.replace(&edit.old_text, &edit.new_text))
}
