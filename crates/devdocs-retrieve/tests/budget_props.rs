//! Budget packer properties (docs/implementation/04 §Properties): the
//! packer never exceeds the budget after rank 1 and always includes rank 1;
//! `truncated` and `next_cursor` report the cut correctly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_retrieve::budget::{Packed, pack};
use devdocs_store::Hit;
use proptest::prelude::*;

/// Builds a hit from the proptest's token count and id.
fn hit(chunk_id: u64, token_count: u32) -> Hit {
    devdocs_testkit::sample_hit(chunk_id.cast_signed(), token_count)
}

/// The budget bounds: rank 1 itself may exceed the budget — the oversize
/// case is chunked at open time, never dropped (docs/implementation/08
/// §Fusion and budget) — so the tail and the total stay within
/// `max_tokens` and `max(max_tokens, rank 1)` respectively.
fn assert_budget_bounds(packed: &Packed, max_tokens: u64) -> Result<(), TestCaseError> {
    let rank_one: u64 = packed
        .items
        .first()
        .map_or(0, |(hit, _)| u64::from(hit.token_count));
    let tail: u64 = packed
        .items
        .iter()
        .skip(1)
        .map(|(hit, _)| u64::from(hit.token_count))
        .sum();
    let total: u64 = packed
        .items
        .iter()
        .map(|(hit, _)| u64::from(hit.token_count))
        .sum();
    prop_assert!(tail <= max_tokens, "rank-1 tail {tail} > {max_tokens}");
    prop_assert!(
        total <= max_tokens.max(rank_one),
        "total {total} > max(max_tokens {max_tokens}, rank 1 {rank_one})"
    );
    Ok(())
}

/// The cut shape: a truncated pack keeps a maximal prefix — it stops at
/// the limit or at the first result that would exceed the budget — and
/// reports the last served chunk id as the resume cursor. An untruncated
/// pack serves the whole fusion order with no cursor.
fn assert_cut_shape(
    packed: &Packed,
    fused: &[(Hit, f64)],
    limit: usize,
    max_tokens: u64,
) -> Result<(), TestCaseError> {
    if fused.is_empty() {
        prop_assert_eq!(packed.items.len(), 0);
        prop_assert_eq!(packed.truncated, false);
        prop_assert_eq!(packed.next_cursor.clone(), None);
        return Ok(());
    }
    if !packed.truncated {
        prop_assert_eq!(packed.items.len(), fused.len());
        prop_assert_eq!(packed.next_cursor.clone(), None);
        return Ok(());
    }
    let kept = packed.items.len();
    prop_assert!(kept < fused.len());
    let used: u64 = packed
        .items
        .iter()
        .map(|(hit, _)| u64::from(hit.token_count))
        .sum();
    let next: u64 = u64::from(fused[kept].0.token_count);
    prop_assert!(kept == limit || used + next > max_tokens, "cut too early");
    prop_assert_eq!(
        packed.next_cursor.clone(),
        Some(packed.items.last().unwrap().0.chunk_id.to_string())
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The packer's four properties hold for every input.
    #[test]
    fn packer_properties_hold(
        costs in prop::collection::vec(0u32..1_000_000, 0..64),
        max_tokens in 0u64..200_000,
        limit in 1usize..=64,
    ) {
        let fused: Vec<_> = costs
            .iter()
            .enumerate()
            .map(|(i, &cost)| (hit(i as u64 + 1, cost), 1.0 / (f64::from(u32::try_from(i).unwrap_or(u32::MAX)) + 1.0)))
            .collect();
        let packed = pack(fused.clone(), limit, max_tokens);

        // Rank 1 always fits: the first result is never dropped.
        if !fused.is_empty() {
            prop_assert_eq!(packed.items[0].0.chunk_id, fused[0].0.chunk_id);
        }

        // The kept results are a prefix of the fusion order.
        for (kept, expected) in packed.items.iter().zip(fused.iter()) {
            prop_assert_eq!(kept.0.chunk_id, expected.0.chunk_id);
        }

        assert_budget_bounds(&packed, max_tokens)?;
        assert_cut_shape(&packed, &fused, limit, max_tokens)?;
    }
}
