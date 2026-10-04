//! Shared budgets for paginated text tools. Per-tool scan and result limits
//! stay with the tool that owns them.

/// Maximum text payload per page; JSON metadata may add a small envelope.
pub(crate) const MAX_TEXT_OUTPUT_BYTES: usize = 32 * 1024;

/// Leave room for pagination and scan metadata when collecting JSON entries.
pub(crate) const OUTPUT_METADATA_RESERVE_BYTES: usize = 1024;

/// Bound directory traversal even when few entries match the requested page.
pub(crate) const MAX_DIRECTORY_ENTRIES: usize = 20_000;
