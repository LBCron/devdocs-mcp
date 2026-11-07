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

/// Whether this build carries the grammar for `lang`: without its feature
/// the extraction degrades to empty, so each language fixture branches on
/// it and pins the degrade behavior in every feature state.
fn grammar_enabled(lang: &str) -> bool {
    grammar_for(lang).is_some()
}

/// The Rust grammar extracts the module's declarations in source order.
/// Impl members carry `method`; the trait's `run` declaration and the
/// free functions stay `function`.
#[test]
fn rust_fixture_extracts_declarations_in_source_order() {
    let code = include_str!("fixtures/symbols/rust.sample");
    let symbols = extract("rust", code);
    if !grammar_enabled("rust") {
        assert!(
            symbols.is_empty(),
            "without the grammar the extraction degrades to empty"
        );
        return;
    }
    assert_eq!(
        pairs(&symbols),
        vec![
            ("MAX_RETRIES", "constant"),
            ("Command", "struct"),
            ("execute", "method"),
            ("RunError", "enum"),
            ("Executor", "trait"),
            ("run", "function"),
            ("LocalExecutor", "struct"),
            ("run", "method"),
            ("TargetKind", "type"),
            ("RELOAD_LOCK", "constant"),
            ("deploy", "module"),
            ("plan", "function"),
            ("parse_config", "function"),
        ]
    );
    assert_eq!(symbols[0].line, 6, "lines are 1-based within the block");
}

/// The Go grammar extracts the declarations; the method stays a method
/// and the `interface` type declaration carries the `interface` kind.
#[test]
fn go_fixture_extracts_declarations_in_source_order() {
    let code = include_str!("fixtures/symbols/go.sample");
    let symbols = extract("golang", code);
    if !grammar_enabled("golang") {
        assert!(
            symbols.is_empty(),
            "without the grammar the extraction degrades to empty"
        );
        return;
    }
    assert_eq!(
        pairs(&symbols),
        vec![
            ("ErrRejected", "variable"),
            ("MaxRetries", "constant"),
            ("Target", "type"),
            ("Planner", "interface"),
            ("DefaultPlanner", "type"),
            ("Client", "type"),
            ("Handler", "interface"),
            ("Plan", "method"),
            ("mu", "variable"),
            ("cacheVersion", "constant"),
            ("PlanAll", "function"),
        ]
    );
}

/// The Python grammar extracts classes and their methods; decorated
/// class members stay methods, module-level assignments stay out, and
/// `async def` — bare and under a decorator — extracts like a plain
/// function (the `decorated_definition` wrapper is descended).
#[test]
fn python_fixture_extracts_classes_methods_and_functions() {
    let code = include_str!("fixtures/symbols/python.sample");
    let symbols = extract("py", code);
    if !grammar_enabled("py") {
        assert!(
            symbols.is_empty(),
            "without the grammar the extraction degrades to empty"
        );
        return;
    }
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
            ("fetch_plan", "function"),
            ("cached", "function"),
            ("refresh", "function"),
        ]
    );
}

/// The JavaScript grammar extracts declarations, classes with their
/// methods, and function- or class-valued variables.
#[test]
fn javascript_fixture_extracts_declarations_classes_and_variables() {
    let code = include_str!("fixtures/symbols/javascript.sample");
    let symbols = extract("jsx", code);
    if !grammar_enabled("jsx") {
        assert!(
            symbols.is_empty(),
            "without the grammar the extraction degrades to empty"
        );
        return;
    }
    assert_eq!(
        pairs(&symbols),
        vec![
            ("MAX_RETRIES", "variable"),
            ("cacheVersion", "variable"),
            ("makeWidget", "variable"),
            ("tool", "variable"),
            ("withDefault", "variable"),
            ("restProps", "variable"),
            ("first", "variable"),
            ("remaining", "variable"),
            ("version", "variable"),
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
            ("regenerate", "function"),
        ]
    );
}

/// A JavaScript `variable_declarator` is indexed only at module scope:
/// `for`-statement initializers and `switch`-case declarations stay out,
/// while plain module-level declarations and `export`ed ones stay in.
/// The transparent non-scoping wrappers — a label, and a single-statement
/// `if` body — keep a `var` declaration at module scope when they
/// themselves sit there, and the same shapes inside a function stay out.
/// A lexical declaration (`let`/`const`) allows neither wrapper (a
/// `SyntaxError` in real JavaScript, but the grammar still parses it).
#[test]
fn javascript_module_scope_declarators_only() {
    let code = concat!(
        "let top = 1;\n",
        "export const exported = 2;\n",
        "var alsoTop = 3;\n",
        "outer: var labeled = 4;\n",
        "outer: let labeledLet = 11;\n",
        "if (top) var conditional = 5;\n",
        "if (top) let blocked = 10;\n",
        "for (let i = 0; i < 1; i++) {}\n",
        "for (var j = 0; j < 1; j++) {}\n",
        "switch (top) {\n",
        "  case 1:\n",
        "    let inner = 6;\n",
        "    var innerVar = 7;\n",
        "}\n",
        "function f() {\n",
        "  inner: var hidden = 8;\n",
        "  if (top) var buried = 9;\n",
        "}\n",
    );
    let symbols = extract("javascript", code);
    if !grammar_enabled("javascript") {
        assert!(
            symbols.is_empty(),
            "without the grammar the extraction degrades to empty"
        );
        return;
    }
    assert_eq!(
        pairs(&symbols),
        vec![
            ("top", "variable"),
            ("exported", "variable"),
            ("alsoTop", "variable"),
            ("labeled", "variable"),
            ("conditional", "variable"),
            ("f", "function"),
        ]
    );
}

/// A Rust declaration inside an `impl_item` body is a method; a trait
/// default method and a function nested in a method body stay functions
/// (docs/implementation/06 §Symbol columns).
#[test]
fn rust_impl_members_are_methods() {
    let symbols = extract(
        "rust",
        "trait T {\n    fn provided(&self) {}\n}\n\nimpl T for S {\n    fn provided(&self) {}\n}\n\nfn free() {}\n",
    );
    if !grammar_enabled("rust") {
        assert!(
            symbols.is_empty(),
            "without the grammar the extraction degrades to empty"
        );
        return;
    }
    assert_eq!(
        pairs(&symbols),
        vec![
            ("T", "trait"),
            ("provided", "function"),
            ("provided", "method"),
            ("free", "function"),
        ]
    );

    // A function nested in a method body is not a method.
    let symbols = extract(
        "rust",
        "struct S;\nimpl S {\n    fn outer() {\n        fn inner() {}\n    }\n}\n",
    );
    assert_eq!(
        pairs(&symbols),
        vec![("S", "struct"), ("outer", "method"), ("inner", "function")]
    );
}

/// A language outside the grammar set yields nothing; an alias resolves
/// to the same grammar as its canonical name.
#[test]
fn unknown_languages_and_aliases() {
    assert_eq!(extract("cobol", "MOVE A TO B."), Vec::new());
    if grammar_enabled("rust") {
        assert_eq!(extract("rust", "fn main() {}").len(), 1);
        assert!(grammar_for("rust").is_some());
    }
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
