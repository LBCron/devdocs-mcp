//! Reciprocal Rank Fusion (docs/implementation/08 §Fusion and budget).
//!
//! `score(d) = Σ_rungs 1 / (k + rank_rung(d))` with the k=60 default from
//! Cormack et al., SIGIR 2009. Ties break by lower chunk id, a
//! property-tested stable order. Fusion with one rung is identity: the
//! rung order is already strictly decreasing in score.

use std::collections::HashMap;

use devdocs_store::Hit;

/// The default RRF constant (docs/implementation/08: k = 60).
pub const RRF_K_DEFAULT: u32 = 60;

/// Fuses the rung result lists into one scored, ordered list.
///
/// Each rung list must already be in its own best-first order; positions
/// become the ranks (1-based). A chunk seen in several rungs accumulates
/// its scores — summed in a canonical order, so the fused output is
/// bit-identical under rung reordering; its [`Hit`] comes from the first
/// rung that carried it. Output order: score descending, ties by lower
/// chunk id — byte-identical for identical rung inputs.
#[must_use]
pub fn fuse(k: u32, rungs: &[Vec<Hit>]) -> Vec<(Hit, f64)> {
    let mut contributions: HashMap<i64, Vec<f64>> = HashMap::new();
    let mut hits: HashMap<i64, Hit> = HashMap::new();
    for rung in rungs {
        for (position, hit) in rung.iter().enumerate() {
            let rank = f64::from(u32::try_from(position + 1).unwrap_or(u32::MAX));
            contributions
                .entry(hit.chunk_id)
                .or_default()
                .push(1.0 / (f64::from(k) + rank));
            hits.entry(hit.chunk_id).or_insert_with(|| hit.clone());
        }
    }
    let mut fused: Vec<(Hit, f64)> = contributions
        .into_iter()
        .filter_map(|(chunk_id, mut parts)| {
            // Canonical order: equal ranks produce equal contributions, so
            // sorting before the sum is permutation-invariant.
            parts.sort_by(f64::total_cmp);
            let score = parts.iter().sum();
            hits.remove(&chunk_id).map(|hit| (hit, score))
        })
        .collect();
    fused.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.chunk_id.cmp(&b.0.chunk_id))
    });
    fused
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::fuse;
    use devdocs_store::Hit;

    /// A minimal hit carrying just the identity fields fusion orders by.
    fn hit(chunk_id: i64, library: &str) -> Hit {
        Hit {
            chunk_id,
            source_id: chunk_id,
            library: library.to_owned(),
            version: "1.0.0".to_owned(),
            section_path: vec![],
            symbol: None,
            symbol_kind: None,
            snippet: None,
            text: String::new(),
            token_count: 1,
            rank: None,
            content_hash: [0u8; 32],
            source_url: String::new(),
            source_commit: None,
            license_status: "approved_index".to_owned(),
        }
    }

    /// Fusion with one rung is identity: the rung's best-first order
    /// survives unchanged.
    #[test]
    fn single_rung_fusion_is_identity() {
        let rung = vec![hit(3, "a"), hit(1, "a"), hit(2, "a")];
        let fused = fuse(60, &[rung]);
        let ids: Vec<i64> = fused.iter().map(|(h, _)| h.chunk_id).collect();
        assert_eq!(ids, vec![3, 1, 2]);
    }

    /// A chunk in two rungs accumulates both RRF contributions and wins.
    #[test]
    fn multi_rung_scores_accumulate() {
        let rung_a = vec![hit(1, "a"), hit(2, "a")];
        let rung_b = vec![hit(2, "a"), hit(3, "a")];
        let fused = fuse(60, &[rung_a, rung_b]);
        // Chunk 2 ranks second in both rungs: 2/(60+2) beats 1/(60+1).
        let ids: Vec<i64> = fused.iter().map(|(h, _)| h.chunk_id).collect();
        assert_eq!(ids.first(), Some(&2));
    }

    /// Exact score ties break by lower chunk id.
    #[test]
    fn ties_break_by_lower_chunk_id() {
        // Both chunks rank 1 in their own rung — equal scores.
        let rung_a = vec![hit(7, "a")];
        let rung_b = vec![hit(3, "a")];
        let fused = fuse(60, &[rung_a, rung_b]);
        let ids: Vec<i64> = fused.iter().map(|(h, _)| h.chunk_id).collect();
        assert_eq!(ids, vec![3, 7]);
    }

    /// Fusion is deterministic: identical inputs give identical output.
    #[test]
    fn fusion_is_deterministic() {
        let rungs = || {
            vec![
                vec![hit(5, "a"), hit(9, "a"), hit(1, "a")],
                vec![hit(1, "a"), hit(5, "b")],
            ]
        };
        let first = fuse(60, &rungs());
        let second = fuse(60, &rungs());
        assert_eq!(first, second);
    }
}
