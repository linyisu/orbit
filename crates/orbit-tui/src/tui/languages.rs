//! Keep terminal code views on the same language registry as the GUI.
pub(super) use orbit_syntax::{
    SYNTAXES, is_markdown_label, label_key, syntax_for_label, syntax_for_path,
};
