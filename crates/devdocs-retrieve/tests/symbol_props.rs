//! Symbol query normalization properties (docs/implementation/04
//! §Properties): `normalize_symbol` never panics and is idempotent over
//! arbitrary symbol-query strings.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_retrieve::normalize_symbol;
use proptest::prelude::*;

/// Symbol-query-ish strings: printable ASCII words mixed with the
/// bracket, bracket-pair, dot, and call fragments the normalizer strips.
fn symbol_query() -> impl Strategy<Value = String> {
    let fragment = prop::sample::select(vec![
        "makeWidget",
        "obj.method",
        "Router",
        " ",
        ".",
        "(",
        ")",
        "[",
        "]",
        "<",
        ">",
        "()",
        "[]",
        "<>",
        "(name)",
        "[T]",
        "<K, V>",
        "Vec<Map<K, V>>",
        "useRouter()",
        "(((",
        ")))",
        "fn(]",
    ]);
    prop::collection::vec(fragment, 0..12).prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// `normalize_symbol` is total — it never panics — and idempotent:
    /// normalizing twice equals normalizing once (docs/implementation/13
    /// §M5 step 3).
    #[test]
    fn normalize_symbol_is_total_and_idempotent(query in symbol_query()) {
        let once = normalize_symbol(&query);
        let twice = normalize_symbol(&once);
        prop_assert_eq!(twice, once);
    }
}
