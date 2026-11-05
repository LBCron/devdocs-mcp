//! RRF fusion stability properties (docs/implementation/04 §Property
//! tests): the fused output is stable under reordering of the input
//! rungs, and ties break by lower chunk id.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_retrieve::fusion::fuse;
use devdocs_store::Hit;
use proptest::prelude::*;

/// Builds a minimal hit carrying only the fields fusion orders by.
fn hit(chunk_id: i64) -> Hit {
    devdocs_testkit::sample_hit(chunk_id, 1)
}

/// The six orderings of three rungs.
const PERMUTATIONS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The fused output — ids and scores, in order — is identical under
    /// every reordering of the input rungs, and ties break by lower chunk
    /// id (docs/implementation/04 §Property tests, fusion row).
    #[test]
    fn fused_output_is_stable_under_rung_reordering(
        a in prop::collection::vec(1i64..1000, 0..8),
        b in prop::collection::vec(1i64..1000, 0..8),
        c in prop::collection::vec(1i64..1000, 0..8),
    ) {
        // Distinct ids per rung: the store never repeats a chunk inside
        // one rung's result list.
        let rung = |ids: &[i64]| {
            let mut seen = Vec::new();
            let mut hits = Vec::new();
            for id in ids {
                if !seen.contains(id) {
                    seen.push(*id);
                    hits.push(hit(*id));
                }
            }
            hits
        };
        let rungs = vec![rung(&a), rung(&b), rung(&c)];
        let baseline = fuse(60, &rungs);
        let all: [&Vec<i64>; 3] = [&a, &b, &c];

        for order in PERMUTATIONS {
            let reordered: Vec<Vec<Hit>> = order.iter().map(|i| rung(all[*i])).collect();
            let fused = fuse(60, &reordered);
            let base_ids: Vec<i64> = baseline.iter().map(|(hit, _)| hit.chunk_id).collect();
            let fused_ids: Vec<i64> = fused.iter().map(|(hit, _)| hit.chunk_id).collect();
            prop_assert_eq!(base_ids, fused_ids, "id order");
            let base_scores: Vec<u64> =
                baseline.iter().map(|(_, score)| score.to_bits()).collect();
            let fused_scores: Vec<u64> =
                fused.iter().map(|(_, score)| score.to_bits()).collect();
            prop_assert_eq!(base_scores, fused_scores, "scores");
        }

        // Ties break by lower chunk id: equal-score neighbors descend.
        for pair in baseline.windows(2) {
            prop_assert!(
                pair[0].1 > pair[1].1 || pair[0].0.chunk_id < pair[1].0.chunk_id,
                "tie order: {} then {}",
                pair[0].0.chunk_id,
                pair[1].0.chunk_id
            );
        }
    }
}
