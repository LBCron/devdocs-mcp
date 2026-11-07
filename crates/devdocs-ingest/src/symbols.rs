//! Symbol extraction over the fenced code blocks of a documentation page
//! (docs/implementation/01 tech-stack note, tree-sitter paragraph).
//!
//! One tree-sitter grammar per supported language, each behind its own
//! feature (`grammar-rust`, `grammar-go`, `grammar-python`,
//! `grammar-javascript`; aggregated by `symbols`). With a grammar feature
//! off, [`grammar_for`] returns `None` for its language and [`extract`]
//! yields nothing — extraction degrades, it never fails. The TypeScript
//! grammar is deliberately absent (docs/implementation/01: binding version
//! conflict); the JavaScript grammar gives it partial coverage.

use tree_sitter::{Language, Node, Parser};

/// One symbol found in a code block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSymbol {
    /// The declared name, e.g. `parse_config`.
    pub name: String,
    /// The kind vocabulary entry for the declaration — one of `function`,
    /// `method`, `class`, `struct`, `enum`, `trait`, `type`, `interface`,
    /// `constant`, `variable`, `module`.
    pub kind: String,
    /// The 1-based line of the declaration inside the code block.
    pub line: u32,
}

/// The grammar for a fence language name, when this build carries it.
/// Accepts the fence aliases the ingest knows (`golang`, `py`, `js`,
/// `jsx`); the comparison is case-sensitive ASCII, and the caller
/// lower-cases the fence token first.
#[must_use]
pub fn grammar_for(lang: &str) -> Option<Language> {
    match lang {
        #[cfg(feature = "grammar-rust")]
        "rust" => Some(Language::new(tree_sitter_rust::LANGUAGE)),
        #[cfg(feature = "grammar-go")]
        "go" | "golang" => Some(Language::new(tree_sitter_go::LANGUAGE)),
        #[cfg(feature = "grammar-python")]
        "python" | "py" => Some(Language::new(tree_sitter_python::LANGUAGE)),
        #[cfg(feature = "grammar-javascript")]
        "javascript" | "js" | "jsx" => Some(Language::new(tree_sitter_javascript::LANGUAGE)),
        _ => None,
    }
}

/// Extracts the symbols declared in `code`, parsed as `lang`. A language
/// without its grammar feature, an unparsable input, or a parse with
/// syntax errors all yield what the walk can salvage — the call never
/// panics and never fails: fences are untrusted content
/// (docs/implementation/09 §Content filtering).
#[must_use]
pub fn extract(lang: &str, code: &str) -> Vec<ExtractedSymbol> {
    let Some(grammar) = grammar_for(lang) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(code.as_bytes(), None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    visit(tree.root_node(), code.as_bytes(), &mut out, 0);
    out
}

/// The recursion cap of the walk. Fence code is bounded by the page body
/// limit, but a pathological nesting depth must not turn the walk into a
/// stack overflow; 64 covers any real declaration nesting many times over.
const MAX_DEPTH: usize = 64;

/// Walks `node` pre-order, recording the mapped declaration kinds.
fn visit(node: Node<'_>, src: &[u8], out: &mut Vec<ExtractedSymbol>, depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }
    if let Some(kind) = mapped_kind(node) {
        push(node, kind, src, out);
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    for child in children {
        visit(child, src, out, depth + 1);
    }
}

/// The mapped vocabulary kind of one declaration node, when the node kind
/// carries one.
fn mapped_kind(node: Node<'_>) -> Option<&'static str> {
    match node.kind() {
        // Rust `function_item`/`function_signature_item` and the
        // Go/JavaScript declarations carry the `function` kind, except a
        // Rust declaration inside an `impl_item` body, which is a method
        // (inherent or trait impl alike). `impl_item` itself carries no
        // `name` and stays unmapped.
        "function_item"
        | "function_signature_item"
        | "function_declaration"
        | "generator_function_declaration" => Some(if rust_impl_member(node) {
            "method"
        } else {
            "function"
        }),
        // Rust `type_item`, the Go `type_spec`, and the Go `type_alias`
        // (all under the enclosing type declaration node); a Go node whose
        // type child is an `interface_type` is an interface declaration
        // (docs/implementation/06 §Symbol columns).
        "type_item" | "type_spec" | "type_alias" => Some(go_type_kind(node)),
        // Python: a class-body `function_definition` is a method.
        "function_definition" => Some(if python_class_member(node) {
            "method"
        } else {
            "function"
        }),
        // JavaScript: module-scope `variable_declarator`s only; a
        // function- or class-valued declarator keeps that kind.
        "variable_declarator" if javascript_top_level(node) => Some(javascript_variable_kind(node)),
        kind => simple_kind(kind),
    }
}

/// The node kinds that map to one fixed vocabulary entry regardless of
/// context: the Rust `struct_item`/`enum_item`/`trait_item` shape, the Rust
/// `const_item`/`static_item` and Go `const_spec` constants, the Rust
/// `mod_item` module, the method declarations, the Go `var_spec` variable,
/// and the class definitions.
const SIMPLE_KINDS: &[(&str, &str)] = &[
    ("struct_item", "struct"),
    ("enum_item", "enum"),
    ("trait_item", "trait"),
    ("const_item", "constant"),
    ("static_item", "constant"),
    ("const_spec", "constant"),
    ("mod_item", "module"),
    ("method_declaration", "method"),
    ("method_definition", "method"),
    ("var_spec", "variable"),
    ("class_definition", "class"),
    ("class_declaration", "class"),
];

/// The fixed kind of a node kind, when one exists.
fn simple_kind(kind: &str) -> Option<&'static str> {
    SIMPLE_KINDS
        .iter()
        .find(|(node_kind, _)| *node_kind == kind)
        .map(|(_, mapped)| *mapped)
}

/// Records one declaration: its `name` field child and its start line.
/// A destructuring pattern name (`const { a, b } = ...`) records one
/// entry per bound identifier. A node without a usable `name` (a
/// parse-error hole) contributes nothing.
fn push(node: Node<'_>, kind: &str, src: &[u8], out: &mut Vec<ExtractedSymbol>) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let mut names = Vec::new();
    if matches!(name.kind(), "object_pattern" | "array_pattern") {
        pattern_bindings(name, src, &mut names);
    } else if let Ok(text) = name.utf8_text(src)
        && !text.is_empty()
    {
        names.push(text);
    }
    let line = u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX);
    for text in names {
        out.push(ExtractedSymbol {
            name: text.to_owned(),
            kind: kind.to_owned(),
            line,
        });
    }
}

/// The identifiers a destructuring pattern binds, in source order.
/// Structural only: an assignment pattern contributes its `left`, a
/// `pair_pattern` its `value`, so default-value expressions and object
/// keys stay out.
fn pattern_bindings<'a>(pattern: Node<'_>, src: &'a [u8], out: &mut Vec<&'a str>) {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            if let Ok(text) = pattern.utf8_text(src)
                && !text.is_empty()
            {
                out.push(text);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(left) = pattern.child_by_field_name("left") {
                pattern_bindings(left, src, out);
            }
        }
        "pair_pattern" => {
            if let Some(value) = pattern.child_by_field_name("value") {
                pattern_bindings(value, src, out);
            }
        }
        "rest_pattern" | "object_pattern" | "array_pattern" => {
            let mut cursor = pattern.walk();
            for child in pattern.children(&mut cursor) {
                pattern_bindings(child, src, out);
            }
        }
        _ => {}
    }
}

/// The kind of a mapped type declaration: a Go `type_spec` or `type_alias`
/// whose `type` field child is an `interface_type` node is an `interface`;
/// everything else (including every Rust `type_item`, whose `type` child is
/// never an `interface_type`) stays a `type`.
fn go_type_kind(node: Node<'_>) -> &'static str {
    if node
        .child_by_field_name("type")
        .is_some_and(|child| child.kind() == "interface_type")
    {
        "interface"
    } else {
        "type"
    }
}

/// Whether a Rust declaration is a member of an `impl_item` body: its
/// direct parent is the impl's `declaration_list`. A declaration anywhere
/// else — a free function, a trait default method, a function nested in a
/// method body — stays a `function`.
fn rust_impl_member(node: Node<'_>) -> bool {
    matches!(node.parent(), Some(parent) if parent.kind() == "declaration_list"
        && parent.parent().is_some_and(|impl_node| impl_node.kind() == "impl_item"))
}

/// Whether a Python `function_definition` is a member of a class body:
/// its first non-wrapper ancestors reach a `class_definition`
/// (`decorated_definition` and the class `block` are wrappers). A function
/// nested in another function, however deep, is not a method.
fn python_class_member(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "block" | "decorated_definition" => current = parent.parent(),
            "class_definition" => return true,
            _ => return false,
        }
    }
    false
}

/// Whether a JavaScript `variable_declarator` sits at module scope: its
/// declaration statement, climbed past the transparent non-scoping
/// wrappers (`labeled_statement` for both declaration kinds, a
/// single-statement `if_statement` body for `var`, which alone hoists
/// out of an `if`), must reach the `program` or an `export_statement`.
/// Everything else — a function body, a `for`-statement initializer, a
/// `switch`-case declaration — is not an indexable module symbol.
fn javascript_top_level(node: Node<'_>) -> bool {
    let Some(declaration) = node.parent() else {
        return false;
    };
    if !matches!(
        declaration.kind(),
        "lexical_declaration" | "variable_declaration"
    ) {
        return false;
    }
    let hoists = declaration.kind() == "variable_declaration";
    let mut ancestor = declaration.parent();
    while let Some(wrapper) = ancestor {
        match wrapper.kind() {
            "labeled_statement" => ancestor = wrapper.parent(),
            "if_statement" if hoists => ancestor = wrapper.parent(),
            kind => return matches!(kind, "program" | "export_statement"),
        }
    }
    false
}

/// The kind of a JavaScript `variable_declarator`: a function or class
/// value makes it a declaration of that kind, everything else a variable.
/// The expression-form generator (`function* () {...}`) is a
/// `generator_function` node and stays a `function` too.
fn javascript_variable_kind(node: Node<'_>) -> &'static str {
    match node.child_by_field_name("value").map(|value| value.kind()) {
        Some("function_expression" | "arrow_function" | "function" | "generator_function") => {
            "function"
        }
        Some("class") => "class",
        _ => "variable",
    }
}
