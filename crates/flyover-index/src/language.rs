//! Language detection by file name. Cheap and deterministic.
//!
//! M1 extends this with shebang sniffing and wires each language to its tree-sitter grammar.
//! Names follow GitHub Linguist so colors and legends can reuse its palette.

use std::path::Path;

pub const UNKNOWN: &str = "Other";

/// Exact file names that carry no useful extension.
const BY_NAME: &[(&str, &str)] = &[
    ("BUILD", "Starlark"),
    ("BUILD.bazel", "Starlark"),
    ("BUILD.gn", "GN"),
    ("CMakeLists.txt", "CMake"),
    ("Dockerfile", "Dockerfile"),
    ("Makefile", "Makefile"),
    ("WORKSPACE", "Starlark"),
];

/// Lowercase extension to language.
const BY_EXTENSION: &[(&str, &str)] = &[
    ("c", "C"),
    ("cc", "C++"),
    ("cjs", "JavaScript"),
    ("cpp", "C++"),
    ("cs", "C#"),
    ("css", "CSS"),
    ("cxx", "C++"),
    ("dart", "Dart"),
    ("ex", "Elixir"),
    ("exs", "Elixir"),
    ("gn", "GN"),
    ("gni", "GN"),
    ("go", "Go"),
    ("h", "C"),
    ("hh", "C++"),
    ("hpp", "C++"),
    ("hs", "Haskell"),
    ("html", "HTML"),
    ("java", "Java"),
    ("js", "JavaScript"),
    ("json", "JSON"),
    ("jsx", "JavaScript"),
    ("kt", "Kotlin"),
    ("lua", "Lua"),
    ("m", "Objective-C"),
    ("md", "Markdown"),
    ("mjs", "JavaScript"),
    ("mm", "Objective-C++"),
    ("mts", "TypeScript"),
    ("php", "PHP"),
    ("prisma", "Prisma"),
    ("proto", "Protocol Buffer"),
    ("py", "Python"),
    ("rb", "Ruby"),
    ("rs", "Rust"),
    ("scala", "Scala"),
    ("scss", "SCSS"),
    ("sh", "Shell"),
    ("sql", "SQL"),
    ("swift", "Swift"),
    ("toml", "TOML"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("vue", "Vue"),
    ("wgsl", "WGSL"),
    ("xml", "XML"),
    ("yaml", "YAML"),
    ("yml", "YAML"),
    ("zig", "Zig"),
];

pub fn detect(path: &Path) -> &'static str {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if let Some((_, lang)) = BY_NAME.iter().find(|(n, _)| *n == name) {
            return lang;
        }
    }
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return UNKNOWN;
    };
    let ext = ext.to_ascii_lowercase();
    BY_EXTENSION
        .iter()
        .find(|(e, _)| *e == ext)
        .map_or(UNKNOWN, |(_, lang)| lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_extension_case_insensitively() {
        assert_eq!(detect(Path::new("src/main.rs")), "Rust");
        assert_eq!(detect(Path::new("a/b/View.TSX")), "TypeScript");
        assert_eq!(detect(Path::new("net/socket.cc")), "C++");
    }

    #[test]
    fn detects_by_file_name() {
        assert_eq!(detect(Path::new("chrome/BUILD.gn")), "GN");
        assert_eq!(detect(Path::new("Dockerfile")), "Dockerfile");
    }

    #[test]
    fn unknown_falls_back_to_other() {
        assert_eq!(detect(Path::new("LICENSE")), UNKNOWN);
        assert_eq!(detect(Path::new("data.xyz")), UNKNOWN);
    }
}
