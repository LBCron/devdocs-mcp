//! The FTS5 MATCH query builder.
//!
//! The builder is a pure, total function: any input string yields a valid
//! MATCH expression (docs/implementation/04 §Properties). Every token is
//! double-quoted with embedded quotes doubled, so FTS5 operators (`AND`,
//! `OR`, `NOT`, `NEAR`, `-`, `*`, `^`, `:`) lose their power and hyphenated
//! code tokens like `use-route` stay whole; tokens are joined with implicit
//! ANDs.

/// The expression for input with no usable tokens: an empty quoted phrase,
/// which parses and matches nothing.
const NO_MATCH_EXPR: &str = "\"\"";

/// Builds a valid FTS5 MATCH expression from arbitrary user input.
///
/// Every token is double-quoted with embedded quotes doubled, so operator
/// words become literal phrase queries and hyphenated code tokens like
/// `use-route` stay whole. NUL bytes are dropped — FTS5 parses the
/// expression through a NUL-terminated C string, so they would truncate it
/// mid-phrase. Input with no usable tokens becomes the empty quoted phrase
/// `""`, a valid expression that matches nothing.
#[must_use]
pub fn build_match_query(input: &str) -> std::borrow::Cow<'_, str> {
    let mut quoted: Vec<String> = Vec::new();
    for token in input.split_whitespace() {
        let token = token.replace('\0', "");
        if token.is_empty() {
            continue;
        }
        quoted.push(format!("\"{}\"", token.replace('"', "\"\"")));
    }
    if quoted.is_empty() {
        return std::borrow::Cow::Borrowed(NO_MATCH_EXPR);
    }
    std::borrow::Cow::Owned(quoted.join(" "))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::build_match_query;

    /// Empty and whitespace-only input become the no-match expression.
    #[test]
    fn empty_input_becomes_the_no_match_expression() {
        assert_eq!(build_match_query(""), "\"\"");
        assert_eq!(build_match_query("   \t\n "), "\"\"");
    }

    /// Operator words become quoted literals, so they cannot reshape the
    /// query structure.
    #[test]
    fn operator_words_become_quoted_literals() {
        assert_eq!(build_match_query("AND"), "\"AND\"");
        assert_eq!(build_match_query("a AND b"), "\"a\" \"AND\" \"b\"");
        assert_eq!(
            build_match_query("(a OR b) NOT c"),
            "\"(a\" \"OR\" \"b)\" \"NOT\" \"c\""
        );
    }

    /// Embedded double quotes are doubled, keeping the expression a single
    /// valid string literal per token.
    #[test]
    fn embedded_quotes_are_doubled() {
        assert_eq!(build_match_query("say \"hi\""), "\"say\" \"\"\"hi\"\"\"");
        assert_eq!(build_match_query("say\"hi"), "\"say\"\"hi\"");
    }

    /// Hyphenated tokens keep their shape after quoting.
    #[test]
    fn hyphenated_tokens_keep_their_shape() {
        assert_eq!(build_match_query("use-route"), "\"use-route\"");
    }
}
