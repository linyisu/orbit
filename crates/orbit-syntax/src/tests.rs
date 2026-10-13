use super::*;

#[test]
fn hdl_fences_keep_verilog_and_systemverilog_grammars_distinct() {
    for label in ["verilog", "Verilog", "language-verilog", "module.v"] {
        assert_eq!(syntax_for_label(label).unwrap().name, "Verilog", "{label}");
    }
    for label in [
        "system-verilog",
        "SystemVerilog",
        "systemverilog",
        "language-systemverilog",
        "{.system-verilog}",
        "systemverilog title=top_module",
        "module.sv",
    ] {
        assert_eq!(
            syntax_for_label(label).unwrap().name,
            "SystemVerilog",
            "{label}"
        );
    }
}

#[test]
fn every_catalog_target_exists_and_all_ids_and_aliases_resolve() {
    for language in &REGISTRY.languages {
        if let Some(target) = &language.syntect {
            assert!(
                SYNTAXES.find_syntax_by_name(target).is_some(),
                "{}: missing {target}",
                language.id
            );
        }

        for label in std::iter::once(&language.id)
            .chain(std::iter::once(&language.name))
            .chain(&language.aliases)
            .chain(&language.compatibility_aliases)
        {
            let actual = syntax_for_label(label).map(|syntax| syntax.name.as_str());
            assert_eq!(
                actual,
                language.syntect.as_deref(),
                "{}: {label}",
                language.id
            );
            assert_eq!(label_key(label), language.id, "{label}");
        }
    }
}

#[test]
fn badge_identity_preserves_languages_without_highlighting_and_ignores_attributes() {
    for label in ["rust", "RS", "{.rs}", "language-RUST", "rs title=Orbit"] {
        assert_eq!(label_key(label), "rust", "{label}");
    }
    for label in ["powershell", "pwsh", "PowerShell"] {
        assert_eq!(label_key(label), "powershell", "{label}");
    }
    assert_eq!(label_key(""), "text");
    assert_eq!(
        label_key("invented-language title=Orbit"),
        "invented-language"
    );
    assert_eq!(label_key("v"), "v");
}

#[test]
fn unsupported_catalog_languages_do_not_fall_through_to_unrelated_extensions() {
    // Syntect associates .v with Verilog, but Shiki's language ID v means V.
    assert!(syntax_for_label("v").is_none());
    assert_eq!(syntax_for_label("module.v").unwrap().name, "Verilog");
    assert!(syntax_for_label("angular-html").is_none());
    assert!(syntax_for_label("invented-language").is_none());
}

#[test]
fn markdown_aliases_and_attributes_share_the_catalog() {
    for label in [
        "md",
        "markdown",
        "mdx",
        "gfm",
        "commonmark",
        "{.md}",
        "language-md",
        "md title=example",
    ] {
        assert!(is_markdown_label(label), "{label}");
    }

    assert!(!is_markdown_label("rust"));
    assert!(!is_markdown_label("unknown"));
}

#[test]
fn file_types_resolve_paths_with_spaces_and_preserve_extension_meanings() {
    for path in ["docs/Orbit Guide.MD", "README.markdown"] {
        assert_eq!(syntax_for_path(path).unwrap().name, "Markdown", "{path}");
    }
    assert_eq!(syntax_for_path("src/markdown.rs").unwrap().name, "Rust");
    assert!(syntax_for_path("docs/notes.mdx").is_none());
    assert_eq!(
        syntax_for_path("docs.md/notes.txt").unwrap().name,
        "Plain Text"
    );
    for path in ["src/Orbit Code.RS", r"C:\Orbit Code\main.rs"] {
        assert_eq!(syntax_for_path(path).unwrap().name, "Rust", "{path}");
    }
    assert_eq!(
        syntax_for_path("src/Orbit Module.v").unwrap().name,
        "Verilog"
    );
    assert!(syntax_for_path("Dockerfile").is_some());
    assert!(syntax_for_path("Makefile").is_some());
}
