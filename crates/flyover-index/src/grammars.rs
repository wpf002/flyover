//! Tier-1 symbol and import extraction through tree-sitter.
//!
//! Each supported language is a [`Grammar`]: the compiled `Language`, a symbols query, and an
//! imports query. Queries are authored here (not pulled from the grammar crates) so the output is
//! ours to keep stable. Symbol queries capture the definition node as `@def.<kind>` and its
//! identifier as `@name`. Import queries capture the module node as `@import`, with an optional
//! `@callee` used to keep only real import calls (`require`, `require_relative`, ...).
//!
//! Extraction never executes repo code; it only parses text. A per-file time budget cancels a
//! parse that runs long, and the file then keeps file-level data only.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use tree_sitter::{
    Language, ParseOptions, Parser, Point, Query, QueryCursor, QueryError, StreamingIterator,
};

/// A symbol definition found in a file. Lines are 1-based.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Symbol {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: String,
    pub name: String,
}

/// A raw import string found in a file. Line is 1-based.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Import {
    pub line: u32,
    pub module: String,
}

/// Outcome of parsing one file: what the index stores and what the text tiles color by.
pub struct Parsed {
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub tokens: Vec<crate::tokens::TokenSpan>,
}

/// A compiled grammar plus its queries.
pub struct Grammar {
    language: Language,
    symbols: Query,
    imports: Query,
    /// Import-call function names to keep (e.g. `require`). Empty means "no callee filter".
    import_callees: &'static [&'static str],
}

#[derive(Debug, thiserror::Error)]
#[error("compiling the {language} {which} query: {source}")]
pub struct BuildError {
    language: &'static str,
    which: &'static str,
    #[source]
    source: QueryError,
}

/// Every grammar Flyover supports, keyed by the grammar name from [`grammar_key`].
pub struct Registry(BTreeMap<&'static str, Grammar>);

impl Registry {
    pub fn get(&self, key: &str) -> Option<&Grammar> {
        self.0.get(key)
    }
}

/// The grammar name for a file. Display language plus extension, so `.tsx` gets the TSX grammar
/// while `.ts` gets TypeScript. Returns `None` for languages without a tier-1 grammar.
pub fn grammar_key<'a>(language: &'a str, extension: Option<&str>) -> &'a str {
    if language == "TypeScript" && extension == Some("tsx") {
        "TSX"
    } else {
        language
    }
}

/// Build every grammar once. Share the result across threads by reference; a `Query` is `Sync`.
pub fn registry() -> Result<Registry, BuildError> {
    let mut map = BTreeMap::new();
    for spec in SPECS {
        let language: Language = (spec.language)();
        let symbols = compile(spec.name, "symbols", &language, spec.symbols)?;
        let imports = compile(spec.name, "imports", &language, spec.imports)?;
        map.insert(
            spec.name,
            Grammar {
                language,
                symbols,
                imports,
                import_callees: spec.import_callees,
            },
        );
    }
    Ok(Registry(map))
}

fn compile(
    language: &'static str,
    which: &'static str,
    lang: &Language,
    source: &str,
) -> Result<Query, BuildError> {
    Query::new(lang, source).map_err(|source| BuildError {
        language,
        which,
        source,
    })
}

/// Parse `source` with `grammar` and pull out symbols and imports. `budget` caps parse time per
/// file; on timeout the parse is abandoned and this returns `None` (file-level data only).
pub fn parse(grammar: &Grammar, source: &str, budget: Option<Duration>) -> Option<Parsed> {
    let mut parser = Parser::new();
    parser.set_language(&grammar.language).ok()?;

    let bytes = source.as_bytes();
    let tree = match budget {
        None => parser.parse(source, None)?,
        Some(limit) => {
            let start = Instant::now();
            let mut progress = |_state: &tree_sitter::ParseState| -> ControlFlow<()> {
                if start.elapsed() >= limit {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let mut input =
                |offset: usize, _pos: Point| -> &[u8] { bytes.get(offset..).unwrap_or(&[]) };
            let options = ParseOptions::new().progress_callback(&mut progress);
            parser.parse_with_options(&mut input, None, Some(options))?
        }
    };

    let root = tree.root_node();
    let mut symbols = collect_symbols(grammar, root, bytes);
    let mut imports = collect_imports(grammar, root, bytes);

    symbols.sort_unstable();
    symbols.dedup();
    imports.sort_unstable();
    imports.dedup();
    let tokens = crate::tokens::spans(&tree, source);

    Some(Parsed {
        symbols,
        imports,
        tokens,
    })
}

fn collect_symbols(grammar: &Grammar, root: tree_sitter::Node, bytes: &[u8]) -> Vec<Symbol> {
    let names = grammar.symbols.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();
    let mut matches = cursor.matches(&grammar.symbols, root, bytes);
    while let Some(m) = matches.next() {
        let mut name: Option<&str> = None;
        let mut kind: Option<&str> = None;
        let mut def: Option<tree_sitter::Node> = None;
        for cap in m.captures() {
            match names[cap.index as usize] {
                "name" => name = cap.node.utf8_text(bytes).ok(),
                other => {
                    if let Some(k) = other.strip_prefix("def.") {
                        kind = Some(k);
                        def = Some(cap.node);
                    }
                }
            }
        }
        if let (Some(name), Some(kind), Some(def)) = (name, kind, def) {
            if !name.is_empty() {
                out.push(Symbol {
                    start_line: def.start_position().row as u32 + 1,
                    end_line: def.end_position().row as u32 + 1,
                    kind: kind.to_string(),
                    name: name.to_string(),
                });
            }
        }
    }
    out
}

fn collect_imports(grammar: &Grammar, root: tree_sitter::Node, bytes: &[u8]) -> Vec<Import> {
    let names = grammar.imports.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();
    let mut matches = cursor.matches(&grammar.imports, root, bytes);
    while let Some(m) = matches.next() {
        let mut module: Option<tree_sitter::Node> = None;
        let mut callee: Option<&str> = None;
        for cap in m.captures() {
            match names[cap.index as usize] {
                "import" => module = Some(cap.node),
                "callee" => callee = cap.node.utf8_text(bytes).ok(),
                _ => {}
            }
        }
        // A match with a `@callee` is an import call (require, ...); keep only allowed callees.
        // A match without one is a plain import statement and is always kept.
        if let Some(c) = callee {
            if !grammar.import_callees.contains(&c) {
                continue;
            }
        }
        if let Some(node) = module {
            if let Ok(raw) = node.utf8_text(bytes) {
                let module = clean_import(raw);
                if !module.is_empty() {
                    out.push(Import {
                        line: node.start_position().row as u32 + 1,
                        module,
                    });
                }
            }
        }
    }
    out
}

/// Strip one layer of surrounding quotes or angle brackets from a raw import token.
fn clean_import(raw: &str) -> String {
    let trimmed = raw.trim();
    let b = trimmed.as_bytes();
    if b.len() >= 2 {
        let pair = (b[0], b[b.len() - 1]);
        let quoted = matches!(
            pair,
            (b'"', b'"') | (b'\'', b'\'') | (b'`', b'`') | (b'<', b'>')
        );
        if quoted {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

/// One language's grammar function and query sources.
struct Spec {
    name: &'static str,
    language: fn() -> Language,
    symbols: &'static str,
    imports: &'static str,
    import_callees: &'static [&'static str],
}

const NO_CALLEE: &[&str] = &[];
const JS_CALLEES: &[&str] = &["require"];
const RUBY_CALLEES: &[&str] = &["require", "require_relative", "load"];

const SPECS: &[Spec] = &[
    Spec {
        name: "Rust",
        language: || tree_sitter_rust::LANGUAGE.into(),
        symbols: RUST_SYMBOLS,
        imports: RUST_IMPORTS,
        import_callees: NO_CALLEE,
    },
    Spec {
        name: "Go",
        language: || tree_sitter_go::LANGUAGE.into(),
        symbols: GO_SYMBOLS,
        imports: GO_IMPORTS,
        import_callees: NO_CALLEE,
    },
    Spec {
        name: "Python",
        language: || tree_sitter_python::LANGUAGE.into(),
        symbols: PYTHON_SYMBOLS,
        imports: PYTHON_IMPORTS,
        import_callees: NO_CALLEE,
    },
    Spec {
        name: "JavaScript",
        language: || tree_sitter_javascript::LANGUAGE.into(),
        symbols: JS_SYMBOLS,
        imports: JS_IMPORTS,
        import_callees: JS_CALLEES,
    },
    Spec {
        name: "TypeScript",
        language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        symbols: TS_SYMBOLS,
        imports: JS_IMPORTS,
        import_callees: JS_CALLEES,
    },
    Spec {
        name: "TSX",
        language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        symbols: TS_SYMBOLS,
        imports: JS_IMPORTS,
        import_callees: JS_CALLEES,
    },
    Spec {
        name: "C",
        language: || tree_sitter_c::LANGUAGE.into(),
        symbols: C_SYMBOLS,
        imports: C_IMPORTS,
        import_callees: NO_CALLEE,
    },
    Spec {
        name: "C++",
        language: || tree_sitter_cpp::LANGUAGE.into(),
        symbols: CPP_SYMBOLS,
        imports: C_IMPORTS,
        import_callees: NO_CALLEE,
    },
    Spec {
        name: "Java",
        language: || tree_sitter_java::LANGUAGE.into(),
        symbols: JAVA_SYMBOLS,
        imports: JAVA_IMPORTS,
        import_callees: NO_CALLEE,
    },
    Spec {
        name: "C#",
        language: || tree_sitter_c_sharp::LANGUAGE.into(),
        symbols: CSHARP_SYMBOLS,
        imports: CSHARP_IMPORTS,
        import_callees: NO_CALLEE,
    },
    Spec {
        name: "Ruby",
        language: || tree_sitter_ruby::LANGUAGE.into(),
        symbols: RUBY_SYMBOLS,
        imports: RUBY_IMPORTS,
        import_callees: RUBY_CALLEES,
    },
    Spec {
        name: "PHP",
        language: || tree_sitter_php::LANGUAGE_PHP.into(),
        symbols: PHP_SYMBOLS,
        imports: PHP_IMPORTS,
        import_callees: NO_CALLEE,
    },
];

const RUST_SYMBOLS: &str = r#"
(function_item name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(union_item name: (type_identifier) @name) @def.struct
(trait_item name: (type_identifier) @name) @def.trait
(mod_item name: (identifier) @name) @def.module
(macro_definition name: (identifier) @name) @def.macro
(type_item name: (type_identifier) @name) @def.type
(const_item name: (identifier) @name) @def.constant
(static_item name: (identifier) @name) @def.constant
"#;

const RUST_IMPORTS: &str = r#"
(use_declaration
  [(identifier) (scoped_identifier) (use_as_clause) (use_list) (scoped_use_list) (use_wildcard)] @import)
"#;

const GO_SYMBOLS: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(method_declaration name: (field_identifier) @name) @def.method
(type_spec name: (type_identifier) @name) @def.type
"#;

const GO_IMPORTS: &str = r#"
(import_spec path: (interpreted_string_literal) @import)
"#;

const PYTHON_SYMBOLS: &str = r#"
(function_definition name: (identifier) @name) @def.function
(class_definition name: (identifier) @name) @def.class
"#;

const PYTHON_IMPORTS: &str = r#"
(import_statement name: (dotted_name) @import)
(import_statement name: (aliased_import name: (dotted_name) @import))
(import_from_statement module_name: (dotted_name) @import)
(import_from_statement module_name: (relative_import) @import)
"#;

const JS_SYMBOLS: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(class_declaration name: (identifier) @name) @def.class
(method_definition name: (property_identifier) @name) @def.method
"#;

const JS_IMPORTS: &str = r#"
(import_statement source: (string (string_fragment) @import))
(call_expression
  function: (identifier) @callee
  arguments: (arguments (string (string_fragment) @import)))
"#;

const TS_SYMBOLS: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(class_declaration name: (type_identifier) @name) @def.class
(abstract_class_declaration name: (type_identifier) @name) @def.class
(method_definition name: (property_identifier) @name) @def.method
(interface_declaration name: (type_identifier) @name) @def.interface
(type_alias_declaration name: (type_identifier) @name) @def.type
(enum_declaration name: (identifier) @name) @def.enum
"#;

const C_SYMBOLS: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @def.function
(struct_specifier name: (type_identifier) @name) @def.struct
(enum_specifier name: (type_identifier) @name) @def.enum
(type_definition declarator: (type_identifier) @name) @def.type
"#;

const C_IMPORTS: &str = r#"
(preproc_include path: (system_lib_string) @import)
(preproc_include path: (string_literal) @import)
"#;

const CPP_SYMBOLS: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @def.function
(function_definition declarator: (function_declarator declarator: (field_identifier) @name)) @def.method
(function_definition declarator: (function_declarator declarator: (qualified_identifier) @name)) @def.method
(class_specifier name: (type_identifier) @name) @def.class
(struct_specifier name: (type_identifier) @name) @def.struct
(enum_specifier name: (type_identifier) @name) @def.enum
(namespace_definition name: (namespace_identifier) @name) @def.namespace
"#;

const JAVA_SYMBOLS: &str = r#"
(class_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(enum_declaration name: (identifier) @name) @def.enum
(record_declaration name: (identifier) @name) @def.class
(method_declaration name: (identifier) @name) @def.method
(constructor_declaration name: (identifier) @name) @def.method
"#;

const JAVA_IMPORTS: &str = r#"
(import_declaration (scoped_identifier) @import)
(import_declaration (identifier) @import)
"#;

const CSHARP_SYMBOLS: &str = r#"
(class_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(struct_declaration name: (identifier) @name) @def.struct
(enum_declaration name: (identifier) @name) @def.enum
(record_declaration name: (identifier) @name) @def.class
(method_declaration name: (identifier) @name) @def.method
"#;

const CSHARP_IMPORTS: &str = r#"
(using_directive (qualified_name) @import)
(using_directive (identifier) @import)
"#;

const RUBY_SYMBOLS: &str = r#"
(method name: (identifier) @name) @def.method
(singleton_method name: (identifier) @name) @def.method
(class name: (constant) @name) @def.class
(class name: (scope_resolution) @name) @def.class
(module name: (constant) @name) @def.module
"#;

const RUBY_IMPORTS: &str = r#"
(call
  method: (identifier) @callee
  arguments: (argument_list (string (string_content) @import)))
"#;

const PHP_SYMBOLS: &str = r#"
(function_definition name: (name) @name) @def.function
(method_declaration name: (name) @name) @def.method
(class_declaration name: (name) @name) @def.class
(interface_declaration name: (name) @name) @def.interface
(trait_declaration name: (name) @name) @def.trait
(enum_declaration name: (name) @name) @def.enum
"#;

const PHP_IMPORTS: &str = r#"
(namespace_use_clause (qualified_name) @import)
(namespace_use_clause (name) @import)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_lang(key: &str, source: &str) -> Parsed {
        let reg = registry().expect("queries compile");
        let grammar = reg.get(key).expect("grammar present");
        parse(grammar, source, None).expect("parse succeeds")
    }

    #[test]
    fn every_grammar_compiles() {
        // A query error for any language would surface here as a BuildError.
        registry().expect("all queries compile against their grammars");
    }

    #[test]
    fn rust_symbols_and_imports() {
        let got = parse_lang(
            "Rust",
            "use std::collections::BTreeMap;\nfn main() {}\nstruct Point { x: i32 }\n",
        );
        assert!(got
            .symbols
            .iter()
            .any(|s| s.kind == "function" && s.name == "main"));
        assert!(got
            .symbols
            .iter()
            .any(|s| s.kind == "struct" && s.name == "Point"));
        assert_eq!(got.imports.len(), 1);
        assert_eq!(got.imports[0].module, "std::collections::BTreeMap");
    }

    #[test]
    fn python_imports_are_cleaned() {
        let got = parse_lang("Python", "import os\nfrom a.b import c\n");
        let modules: Vec<_> = got.imports.iter().map(|i| i.module.as_str()).collect();
        assert!(modules.contains(&"os"));
        assert!(modules.contains(&"a.b"));
    }

    #[test]
    fn js_require_and_import_strings() {
        let got = parse_lang(
            "JavaScript",
            "import x from \"./x\";\nconst y = require(\"y\");\nnotrequire(\"z\");\n",
        );
        let modules: Vec<_> = got.imports.iter().map(|i| i.module.as_str()).collect();
        assert!(modules.contains(&"./x"));
        assert!(modules.contains(&"y"));
        // A non-import call with a string arg must not become an import.
        assert!(!modules.contains(&"z"));
    }

    #[test]
    fn go_import_quotes_stripped() {
        let got = parse_lang("Go", "package main\nimport \"fmt\"\n");
        assert_eq!(
            got.imports.iter().map(|i| &i.module).collect::<Vec<_>>(),
            vec!["fmt"]
        );
    }

    #[test]
    fn budget_zero_abandons_parse() {
        // A tiny budget cancels the parse; the caller then keeps file-level data only.
        let reg = registry().unwrap();
        let grammar = reg.get("Rust").unwrap();
        let big = "fn f() {}\n".repeat(20_000);
        assert!(parse(grammar, &big, Some(Duration::from_nanos(1))).is_none());
    }
}
