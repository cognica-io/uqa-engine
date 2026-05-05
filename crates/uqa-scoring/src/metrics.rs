//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Information-retrieval relevance metrics: DCG, NDCG, average
//! precision, and mean average precision. Inputs are graded relevance
//! judgments aligned to a ranked list of doc ids; outputs are scalar
//! quality scores in `[0, 1]` (NDCG) or `[0, +inf)` (DCG).
//!
//! Used by the BEIR-style relevance gate in
//! `crates/uqa-engine/tests/relevance.rs`.

/// Discounted cumulative gain at rank `k`. Standard formulation:
/// `DCG@k = sum_{i=1..=k} (2^{rel_i} - 1) / log2(i + 1)`.
///
/// `relevances` is the list of graded relevance values for the
/// retrieval result, in rank order. Anything past index `k - 1` is
/// ignored.
pub fn dcg_at_k(relevances: &[f64], k: usize) -> f64 {
    let n = relevances.len().min(k);
    let mut acc = 0.0;
    for (i, rel) in relevances.iter().take(n).enumerate() {
        let gain = (2f64.powf(*rel)) - 1.0;
        let discount = ((i + 2) as f64).log2();
        acc += gain / discount;
    }
    acc
}

/// Normalized DCG at `k`: `DCG@k / IDCG@k` where `IDCG@k` is the DCG
/// of `relevances` sorted in descending order. Returns 0.0 when the
/// ideal ranking is empty (no graded judgments).
pub fn ndcg_at_k(relevances: &[f64], k: usize) -> f64 {
    let mut ideal = relevances.to_vec();
    ideal.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let idcg = dcg_at_k(&ideal, k);
    if idcg == 0.0 {
        return 0.0;
    }
    dcg_at_k(relevances, k) / idcg
}

/// Average precision at `k`. `is_relevant[i]` is true when result at
/// rank `i + 1` is relevant. Standard definition:
/// `AP@k = (1 / R) * sum_{i=1..=k} rel_i * P@i`,
/// where `R` is the total number of relevant results in the corpus
/// (passed as `total_relevant`).
pub fn average_precision_at_k(is_relevant: &[bool], total_relevant: usize, k: usize) -> f64 {
    if total_relevant == 0 {
        return 0.0;
    }
    let n = is_relevant.len().min(k);
    let mut hits: f64 = 0.0;
    let mut acc: f64 = 0.0;
    for (i, rel) in is_relevant.iter().take(n).enumerate() {
        if *rel {
            hits += 1.0;
            let precision_at_i = hits / (i + 1) as f64;
            acc += precision_at_i;
        }
    }
    acc / total_relevant as f64
}

/// Mean average precision over many queries. `per_query` provides
/// `(is_relevant, total_relevant)` pairs.
pub fn mean_average_precision_at_k(per_query: &[(Vec<bool>, usize)], k: usize) -> f64 {
    if per_query.is_empty() {
        return 0.0;
    }
    let n = per_query.len() as f64;
    let sum: f64 = per_query
        .iter()
        .map(|(rel, total)| average_precision_at_k(rel, *total, k))
        .sum();
    sum / n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "expected {a} ~ {b} within {tol}");
    }

    #[test]
    fn dcg_matches_known_value() {
        // Classic worked example: relevances [3, 2, 3, 0, 1, 2].
        // DCG@6 = (2^3-1)/log2(2) + (2^2-1)/log2(3) + (2^3-1)/log2(4)
        //       + 0 + (2^1-1)/log2(6) + (2^2-1)/log2(7)
        //       = 7 + 3/1.5849... + 7/2 + 0 + 1/2.5849... + 3/2.8073...
        //       ~= 7 + 1.8929 + 3.5 + 0 + 0.3868 + 1.0686
        //       ~= 13.8483.
        let dcg = dcg_at_k(&[3.0, 2.0, 3.0, 0.0, 1.0, 2.0], 6);
        approx_eq(dcg, 13.848, 0.005);
    }

    #[test]
    fn ndcg_perfect_ranking_is_one() {
        let r = vec![3.0, 2.0, 1.0, 0.0];
        approx_eq(ndcg_at_k(&r, 4), 1.0, 1e-9);
    }

    #[test]
    fn ndcg_reversed_ranking_is_below_one() {
        let r = vec![0.0, 1.0, 2.0, 3.0];
        let n = ndcg_at_k(&r, 4);
        assert!(n > 0.0 && n < 1.0, "{n}");
    }

    #[test]
    fn average_precision_known_pattern() {
        // Two relevant items at ranks 1 and 3 out of 5; total relevant = 2.
        // P@1 = 1/1, P@3 = 2/3. AP = (1 + 2/3) / 2 = 0.8333...
        let ap = average_precision_at_k(&[true, false, true, false, false], 2, 5);
        approx_eq(ap, 0.8333, 0.001);
    }

    #[test]
    fn map_averages_per_query() {
        let q1 = (vec![true, false, true], 2);
        let q2 = (vec![false, true, true], 2);
        let map = mean_average_precision_at_k(&[q1, q2], 3);
        // Q1: (1/1 + 2/3)/2 = 0.8333; Q2: (1/2 + 2/3)/2 = 0.5833.
        // MAP = (0.8333 + 0.5833) / 2 = 0.7083.
        approx_eq(map, 0.7083, 0.001);
    }

    #[test]
    fn empty_input_is_zero() {
        assert_eq!(dcg_at_k(&[], 5), 0.0);
        assert_eq!(ndcg_at_k(&[], 5), 0.0);
        assert_eq!(average_precision_at_k(&[], 0, 5), 0.0);
        assert_eq!(mean_average_precision_at_k(&[], 5), 0.0);
    }
}
