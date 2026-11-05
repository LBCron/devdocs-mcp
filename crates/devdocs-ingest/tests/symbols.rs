//! Symbol extraction over the tree-sitter grammar set (M5): per-language
//! fixtures pin the exact extracted `(name, kind)` lists, and the
//! defensive tests pin the never-panics contract over untrusted fence
//! content (docs/implementation/09 §Content filtering).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_ingest::symbols::{extract, grammar_for};

/// The `(name, kind)` pairs of an extraction result.
fn pairs(symbols: &[devdocs_ingest::ExtractedSymbol]) -> Vec<(&str, &str)> {
    symbols
        .iter()
        .map(|symbol| (symbol.name.as_str(), symbol.kind.as_str()))
        .collect()
}

/// The Rust grammar extracts the module's declarations in source order.
#[test]
fn rust_fixture_extracts_declarations_in_source_order() {
    let code = include_str!("fixtures/symbols/rust.sample");
    let symbols = extract("rust", code);
    assert_eq!(
        pairs(&symbols),
        vec![
            ("MAX_RETRIES", "constant"),
            ("Command", "struct"),
            ("execute", "function"),
            ("RunError", "enum"),
            ("Executor", "trait"),
            ("run", "function"),
            ("LocalExecutor", "struct"),
            ("run", "function"),
            ("TargetKind", "type"),
            ("RELOAD_LOCK", "constant"),
            ("deploy", "module"),
            ("plan", "function"),
            ("parse_config", "function"),
        ]
    );
    assert_eq!(symbols[0].line, 6, "lines are 1-based within the block");
}

/// The Go grammar extracts the declarations; the method stays a method.
#[test]
fn go_fixture_extracts_declarations_in_source_order() {
    let code = include_str!("fixtures/symbols/go.sample");
    let symbols = extract("golang", code);
    assert_eq!(
        pairs(&symbols),
        vec![
            ("ErrRejected", "variable"),
            ("MaxRetries", "constant"),
            ("Target", "type"),
            ("Planner", "type"),
            ("DefaultPlanner", "type"),
            ("Plan", "method"),
            ("mu", "variable"),
            ("cacheVersion", "constant"),
            ("PlanAll", "function"),
        ]
    );
}

/// The Python grammar extracts classes and their methods; decorated
/// class members stay methods and module-level assignments stay out.
#[test]
fn python_fixture_extracts_classes_methods_and_functions() {
    let code = include_str!("fixtures/symbols/python.sample");
    let symbols = extract("py", code);
    assert_eq!(
        pairs(&symbols),
        vec![
            ("Planner", "class"),
            ("__init__", "method"),
            ("plan", "method"),
            ("ready", "method"),
            ("ParallelPlanner", "class"),
            ("plan", "method"),
            ("plan_all", "function"),
            ("_retry", "function"),
        ]
    );
}

/// The JavaScript grammar extracts declarations, classes with their
/// methods, and function- or class-valued variables.
#[test]
fn javascript_fixture_extracts_declarations_classes_and_variables() {
    let code = include_str!("fixtures/symbols/javascript.sample");
    let symbols = extract("jsx", code);
    assert_eq!(
        pairs(&symbols),
        vec![
            ("MAX_RETRIES", "variable"),
            ("cacheVersion", "variable"),
            ("Planner", "class"),
            ("constructor", "method"),
            ("plan", "method"),
            ("ready", "method"),
            ("ParallelPlanner", "class"),
            ("plan", "method"),
            ("planAll", "function"),
            ("retry", "function"),
            ("deploy", "function"),
            ("planOne", "function"),
        ]
    );
}

/// A language outside the grammar set yields nothing; an alias resolves
/// to the same grammar as its canonical name.
#[test]
fn unknown_languages_and_aliases() {
    assert_eq!(extract("cobol", "MOVE A TO B."), Vec::new());
    assert_eq!(extract("rust", "fn main() {}").len(), 1);
    assert!(grammar_for("rust").is_some());
    assert_eq!(grammar_for("go"), grammar_for("golang"));
    assert!(grammar_for("typescript").is_none());
}

/// Garbage, truncated, and adversarial inputs never panic; whatever the
/// parser salvages is returned.
#[test]
fn garbage_input_does_not_panic() {
    for code in [
        "",
        "\u{0}\u{1}\u{2}",
        "fn fn fn(",
        "def def(:",
        "func func {",
        "class class class",
        "}}}}}}",
        "let x = ",
        "\u{1F600}\u{1F600} emoji code \u{1F600}",
    ] {
        for lang in ["rust", "go", "python", "javascript"] {
            let _ = extract(lang, code);
        }
    }
}

/// Deeply nested input stays inside the walk's depth cap.
#[test]
fn deeply_nested_input_does_not_panic() {
    let deep = format!("{}fn leaf() {{}}{}", "mod m { ".repeat(80), "}".repeat(80));
    let _ = extract("rust", &deep);
}
