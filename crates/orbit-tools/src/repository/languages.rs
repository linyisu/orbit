use std::path::Path;

pub(crate) fn language(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    let lang = tokei::LanguageType::from_file_extension(ext)?;
    Some(match lang {
        tokei::LanguageType::Rust => "Rust",
        tokei::LanguageType::Python => "Python",
        tokei::LanguageType::JavaScript => "JavaScript",
        tokei::LanguageType::TypeScript => "TypeScript",
        tokei::LanguageType::Go => "Go",
        tokei::LanguageType::Java => "Java",
        tokei::LanguageType::CSharp => "C#",
        tokei::LanguageType::C | tokei::LanguageType::CHeader => "C",
        tokei::LanguageType::Cpp | tokei::LanguageType::CppHeader => "C++",
        tokei::LanguageType::Ruby => "Ruby",
        tokei::LanguageType::Bash => "Shell",
        tokei::LanguageType::Php => "PHP",
        tokei::LanguageType::Kotlin => "Kotlin",
        tokei::LanguageType::Swift => "Swift",
        tokei::LanguageType::Scala => "Scala",
        tokei::LanguageType::Dart => "Dart",
        tokei::LanguageType::Lua => "Lua",
        tokei::LanguageType::Zig => "Zig",
        tokei::LanguageType::Elixir => "Elixir",
        tokei::LanguageType::Haskell => "Haskell",
        tokei::LanguageType::OCaml => "OCaml",
        _ => return None,
    })
}
