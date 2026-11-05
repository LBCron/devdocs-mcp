//! The budget packer (docs/implementation/08 §Fusion and budget).
//!
//! The packer walks results in fusion order, accumulates the stored
//! `token_count` values, and stops before `max_tokens` or once `limit`
//! results are served. Properties:
//!
//! - Rank 1 always fits: the first result is never dropped, even when it
//!   alone exceeds the whole budget — the oversize case is chunked at
//!   `open` time, never dropped. A limit of zero still serves rank 1.
//! - Every result after rank 1 enters only when the accumulated budget
//!   stays within `max_tokens` and the count stays within `limit`
//!   (docs/08: `--limit` caps result count, default 8).
//! - When content remains, the envelope carries `truncated: true` and a
//!   `next_cursor` naming the last served chunk id; resuming continues
//!   after that id in the same fusion order.

use devdocs_store::Hit;

/// The pack outcome: the kept results in fusion order plus the envelope
/// fields the cut produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Packed {
    /// The kept results, in fusion order. Rank 1 is always present when
    /// the input is non-empty.
    pub items: Vec<(Hit, f64)>,
    /// Whether the count cap or the token budget cut any result off.
    pub truncated: bool,
    /// The chunk id of the last served result, when results were cut —
    /// the resume point for the next page.
    pub next_cursor: Option<String>,
}

/// Packs the fused results under the count cap and the token budget.
///
/// `limit` caps the result count (docs/08: `--limit` caps result count);
/// `max_tokens` caps the accumulated `token_count` cost. Rank 1 always
/// serves.
///
/// # Panics
/// Never: the walk is total over any input.
#[must_use]
pub fn pack(fused: Vec<(Hit, f64)>, limit: usize, max_tokens: u64) -> Packed {
    // The count cap never drops rank 1 (docs/08 §Fusion and budget: the
    // packer never silently drops the top result), so a limit of zero
    // still serves the top result.
    let limit = limit.max(1);
    let mut items = Vec::new();
    let mut used: u64 = 0;
    for (index, (hit, score)) in fused.into_iter().enumerate() {
        let cost = u64::from(hit.token_count);
        // The count cap and the budget stop both cut content that
        // remains: the walk stops, reports the cut, and names the last
        // served chunk as the resume point.
        if index >= limit || (index > 0 && used + cost > max_tokens) {
            let truncated = true;
            let next_cursor = items
                .last()
                .map(|(hit, _): &(Hit, f64)| hit.chunk_id.to_string());
            return Packed {
                items,
                truncated,
                next_cursor,
            };
        }
        used += cost;
        items.push((hit, score));
    }
    Packed {
        items,
        truncated: false,
        next_cursor: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::pack;
    use devdocs_store::Hit;

    /// A hit carrying only the fields the packer reads.
    fn hit(chunk_id: i64, token_count: u32) -> Hit {
        devdocs_testkit::sample_hit(chunk_id, token_count)
    }

    /// Results under the budget and the limit all fit; nothing truncates.
    #[test]
    fn everything_under_the_budget_fits() {
        let fused = vec![(hit(1, 10), 1.0), (hit(2, 20), 0.9), (hit(3, 30), 0.8)];
        let packed = pack(fused, 8, 100);
        assert_eq!(packed.items.len(), 3);
        assert!(!packed.truncated);
        assert_eq!(packed.next_cursor, None);
    }

    /// The walk stops before the budget; the cut keeps a cursor naming the
    /// last served chunk.
    #[test]
    fn the_walk_stops_before_the_budget() {
        let fused = vec![(hit(1, 10), 1.0), (hit(2, 20), 0.9), (hit(3, 30), 0.8)];
        let packed = pack(fused, 8, 35);
        // 10 fits, 20 brings the run to 30 ≤ 35, 30 would reach 60 > 35.
        let ids: Vec<i64> = packed.items.iter().map(|(h, _)| h.chunk_id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert!(packed.truncated);
        assert_eq!(packed.next_cursor, Some("2".to_owned()));
    }

    /// The count cap stops the walk even when the budget would fit more;
    /// the cut carries a cursor naming the last served chunk.
    #[test]
    fn the_limit_stops_the_walk() {
        let fused = vec![(hit(1, 10), 1.0), (hit(2, 20), 0.9), (hit(3, 30), 0.8)];
        let packed = pack(fused, 2, 100);
        let ids: Vec<i64> = packed.items.iter().map(|(h, _)| h.chunk_id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert!(packed.truncated);
        assert_eq!(packed.next_cursor, Some("2".to_owned()));
    }

    /// Rank 1 always fits, even when it alone exceeds the whole budget.
    #[test]
    fn rank_one_always_fits() {
        let fused = vec![(hit(1, 5000), 1.0), (hit(2, 10), 0.9)];
        let packed = pack(fused, 8, 100);
        let ids: Vec<i64> = packed.items.iter().map(|(h, _)| h.chunk_id).collect();
        assert_eq!(ids, vec![1]);
        assert!(packed.truncated);
        assert_eq!(packed.next_cursor, Some("1".to_owned()));
    }

    /// A limit of zero still serves rank 1 — the packer never drops the
    /// top result.
    #[test]
    fn a_zero_limit_still_serves_rank_one() {
        let fused = vec![(hit(1, 10), 1.0), (hit(2, 20), 0.9)];
        let packed = pack(fused, 0, 100);
        let ids: Vec<i64> = packed.items.iter().map(|(h, _)| h.chunk_id).collect();
        assert_eq!(ids, vec![1]);
        assert!(packed.truncated);
        assert_eq!(packed.next_cursor, Some("1".to_owned()));
    }

    /// An empty fusion list packs to nothing without truncation.
    #[test]
    fn empty_input_packs_to_nothing() {
        let packed = pack(Vec::new(), 8, 100);
        assert_eq!(packed.items, Vec::new());
        assert!(!packed.truncated);
        assert_eq!(packed.next_cursor, None);
    }
}
