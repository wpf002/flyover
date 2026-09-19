//! Token spans for syntax coloring, taken from the tree-sitter parse tree at index time so the
//! renderer ships no parsers (docs/SPEC.md 2.4).
//!
//! Rather than a highlights query per language, leaves of the parse tree are classified by node
//! kind. Grammars name their nodes consistently enough (`line_comment`, `string_literal`,
//! `integer_literal`, `type_identifier`, ...) that one classifier covers every grammar, including
//! any added later. Anonymous leaves are the grammar's literal tokens: alphabetic ones are
//! keywords, the rest are operators or punctuation.

pub use flyover_tiles::text::TokenSpan;
use tree_sitter::{Node, Tree};

/// What a run of source bytes is, for coloring. Stored in `.ftx` tiles as a u8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TokenClass {
    Other = 0,
    Keyword = 1,
    String = 2,
    Comment = 3,
    Number = 4,
    Type = 5,
    Function = 6,
    Variable = 7,
    Operator = 8,
    Punctuation = 9,
}

impl TokenClass {
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Classify every token of the tree, in source order. Whitespace between spans is implied.
/// Comments and strings are emitted whole rather than descended into: grammars split them into
/// delimiter and content children, and coloring them as one run is what a reader expects.
pub fn spans(tree: &Tree, source: &str) -> Vec<TokenSpan> {
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    let mut visited_children = false;
    loop {
        if !visited_children {
            let node = cursor.node();
            if is_atomic(node.kind()) || node.child_count() == 0 {
                push_leaf(&mut out, node, source);
                visited_children = true;
            } else if cursor.goto_first_child() {
                continue;
            } else {
                visited_children = true;
            }
        }
        if cursor.goto_next_sibling() {
            visited_children = false;
        } else if !cursor.goto_parent() {
            break;
        }
    }
    out.sort_unstable_by_key(|s| s.start);
    out.dedup_by_key(|s| s.start);
    out
}

/// Nodes coloured as a single run, children and all.
fn is_atomic(kind: &str) -> bool {
    kind.contains("comment") || kind.contains("string") || kind.contains("char_literal")
}

fn push_leaf(out: &mut Vec<TokenSpan>, node: Node, source: &str) {
    let (start, end) = (node.start_byte(), node.end_byte());
    if end <= start || end > source.len() {
        return;
    }
    let class = classify(node, source);
    if class == TokenClass::Other && node.is_extra() {
        return;
    }
    out.push(TokenSpan {
        start: start as u32,
        len: (end - start) as u32,
        class: class.as_u8(),
    });
}

fn classify(node: Node, source: &str) -> TokenClass {
    let kind = node.kind();
    if kind.contains("comment") {
        return TokenClass::Comment;
    }
    if kind.contains("string") || kind.contains("char") || kind == "heredoc_body" {
        return TokenClass::String;
    }
    if kind.contains("number")
        || kind.contains("integer")
        || kind.contains("float")
        || kind.contains("decimal")
    {
        return TokenClass::Number;
    }
    if !node.is_named() {
        let text = &source[node.start_byte()..node.end_byte()];
        return if text.chars().all(|c| c.is_alphabetic() || c == '_') && !text.is_empty() {
            TokenClass::Keyword
        } else if text.chars().all(|c| "()[]{},;:.".contains(c)) {
            TokenClass::Punctuation
        } else {
            TokenClass::Operator
        };
    }
    if kind.contains("type") || kind == "constant" || kind == "primitive_type" {
        return TokenClass::Type;
    }
    if kind.ends_with("identifier") || kind == "name" || kind == "word" {
        // A call's callee reads as a function; everything else is a variable.
        let parent = node
            .parent()
            .map(|p| p.kind().to_string())
            .unwrap_or_default();
        return if parent.contains("call")
            || parent.contains("function")
            || parent.contains("method")
        {
            TokenClass::Function
        } else {
            TokenClass::Variable
        };
    }
    if kind == "true" || kind == "false" || kind == "null" || kind == "nil" {
        return TokenClass::Keyword;
    }
    TokenClass::Other
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammars;

    fn spans_for(key: &str, source: &str) -> Vec<TokenSpan> {
        let registry = grammars::registry().unwrap();
        let grammar = registry.get(key).unwrap();
        grammars::parse(grammar, source, None).unwrap().tokens
    }

    fn classes<'a>(source: &'a str, spans: &[TokenSpan], want: TokenClass) -> Vec<&'a str> {
        spans
            .iter()
            .filter(|s| s.class == want.as_u8())
            .map(|s| &source[s.start as usize..(s.start + s.len) as usize])
            .collect()
    }

    #[test]
    fn rust_leaves_are_classified() {
        let src = "// hi\nfn add(a: i32) -> i32 { a + 1 }\n";
        let spans = spans_for("Rust", src);
        assert_eq!(classes(src, &spans, TokenClass::Comment), vec!["// hi"]);
        assert!(classes(src, &spans, TokenClass::Keyword).contains(&"fn"));
        assert!(classes(src, &spans, TokenClass::Number).contains(&"1"));
        assert!(classes(src, &spans, TokenClass::Type).contains(&"i32"));
        assert!(classes(src, &spans, TokenClass::Operator).contains(&"+"));
    }

    #[test]
    fn python_strings_and_comments() {
        let src = "# note\nx = \"hello\"\n";
        let spans = spans_for("Python", src);
        assert_eq!(classes(src, &spans, TokenClass::Comment), vec!["# note"]);
        assert!(classes(src, &spans, TokenClass::String)
            .iter()
            .any(|s| s.contains("hello")));
    }

    #[test]
    fn spans_are_ordered_and_inside_the_source() {
        let src = "fn main() { let s = \"x\"; }\n";
        let spans = spans_for("Rust", src);
        assert!(!spans.is_empty());
        let mut last = 0;
        for s in &spans {
            assert!(s.start >= last, "spans must be sorted");
            assert!((s.start + s.len) as usize <= src.len());
            last = s.start;
        }
    }
}
