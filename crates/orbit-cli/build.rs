use syntect::parsing::SyntaxDefinition;

fn main() {
    println!("cargo:rerun-if-changed=assets/VisualBasic.sublime-syntax");
    let mut builder = two_face::syntax::extra_no_newlines().into_builder();
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
