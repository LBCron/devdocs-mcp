//! The FTS5 rung MATCH expression (docs/implementation/08 §Rungs, rung 2).
//!
//! The M2 store builder (`devdocs_store::build_match_query`) quotes every
//! token and drops operator power; this module layers the rung behavior on
//! top: the last token of each run gains a prefix `*`. Because the M2
//! builder quotes operator words as literal phrases, the whole token
//! sequence is one implicit-AND run — so the run's last token is the
//! expression's last token, and the `*` sits outside the final quoted
//! literal, exactly as the docs/08 example spells it
//! (`"rewrite" "proxy" "request" *`).

/// The prefix marker appended outside the final quoted token.
const PREFIX_MARKER: &str = " *";

/// Builds the FTS5 rung MATCH expression from arbitrary user input.
///
/// Delegates quoting and operator handling to the M2 builder, then appends
/// the prefix `*` to the run's last token. Input with no usable tokens —
/// empty, whitespace, or operator-only after the builder's quoting —
/// yields the builder's valid no-match expression, never an error.
#[must_use]
pub fn rung_match_expr(input: &str) -> std::borrow::Cow<'_, str> {
    let built = devdocs_store::build_match_query(input);
    // The builder's no-match expression is the empty quoted phrase; a
    // prefix star after it is pointless (it still matches nothing), so the
    // no-match form passes through untouched.
    if built.as_ref() == "\"\"" {
        return built;
    }
    std::borrow::Cow::Owned(format!("{built}{PREFIX_MARKER}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::rung_match_expr;

    /// The rung expression quotes tokens through the M2 builder and appends
    /// the prefix star outside the final literal, matching the docs/08
    /// example.
    #[test]
    fn rung_expression_appends_the_prefix_star() {
        assert_eq!(
            rung_match_expr("rewrite proxy request"),
            "\"rewrite\" \"proxy\" \"request\" *"
        );
    }

    /// Empty, whitespace, and operator-only input stay the valid no-match
    /// expression — no star, no error.
    #[test]
    fn empty_input_stays_the_no_match_expression() {
        assert_eq!(rung_match_expr(""), "\"\"");
        assert_eq!(rung_match_expr("   \t\n "), "\"\"");
        assert_eq!(rung_match_expr(" \0 "), "\"\"");
    }

    /// A single token keeps its quotes and gains the star.
    #[test]
    fn single_token_gains_the_star() {
        assert_eq!(rung_match_expr("routing"), "\"routing\" *");
    }

    /// Quoted tokens survive: embedded quotes are doubled by the builder
    /// and the star lands outside the final literal.
    #[test]
    fn embedded_quotes_stay_quoted() {
        assert_eq!(rung_match_expr("say \"hi\""), "\"say\" \"\"\"hi\"\"\" *");
    }
}
