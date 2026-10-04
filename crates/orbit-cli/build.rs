use syntect::parsing::{SyntaxDefinition, SyntaxSetBuilder};

fn main() {
    println!("cargo:rerun-if-changed=assets/VisualBasic.sublime-syntax");
    println!("cargo:rerun-if-changed=assets/MarkdownInline.sublime-syntax");
    let bundled = two_face::syntax::extra_no_newlines().into_builder();
    let inline = SyntaxDefinition::load_from_str(
        include_str!("assets/MarkdownInline.sublime-syntax"),
        false,
        None,
    )
    .expect("valid bundled Markdown inline syntax");
    let mut definitions = bundled.syntaxes().to_vec();
    definitions
        .iter_mut()
        .find(|syntax| syntax.name == "Markdown")
        .expect("bundled Markdown syntax")
        .contexts
        .get_mut("inline")
        .expect("Markdown inline context")
        .patterns
        .splice(0..0, inline.contexts["inject"].patterns.clone());

    // Keep compiled context indices intact: extend patterns without adding or
    // renaming contexts in the existing grammars, and append new grammars last.
    let mut builder = SyntaxSetBuilder::new();
    for syntax in definitions {
        builder.add(syntax);
    }
    builder.add(inline);
    builder.add(
        SyntaxDefinition::load_from_str(
            include_str!("assets/VisualBasic.sublime-syntax"),
            false,
            None,
        )
        .expect("valid bundled VB.NET syntax"),
    );
    let syntax = builder.build();
    let destination =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("syntaxes.bin");
    syntect::dumps::dump_to_uncompressed_file(&syntax, destination)
        .expect("write compiled syntax bundle");
}
