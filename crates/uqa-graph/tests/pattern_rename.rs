//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pattern-rename equivalence (Master Plan Section 2.4).
//!
//! Invariant: `P1 ~ P2` (pattern isomorphism via consistent variable
//! renaming) implies `GMatch_P1 == GMatch_P2`. Concretely, given a
//! pattern `P` with variables `a, b, c`, building a renamed pattern
//! `P'` with `a -> x, b -> y, c -> z` and running both through
//! `GMatch::execute` must produce the same set of matched subgraphs
//! (identical `subgraph_vertices` per match), since pattern matching
//! is structural and the variable names are just labels.

use std::collections::BTreeSet;

use proptest::prelude::*;
use uqa_core::{Edge, Vertex, VertexId};
use uqa_graph::{EdgePattern, GMatch, GraphPattern, GraphStore, MemoryGraphStore, VertexPattern};

const GRAPH: &str = "g";

/// Fixed corpus: 4 person vertices in a small social graph.
/// alice -> bob, bob -> carol, alice -> carol, alice -> dave.
fn corpus() -> MemoryGraphStore {
    let mut g = MemoryGraphStore::new();
    g.create_graph(GRAPH);
    for id in 1..=4 {
        g.add_vertex(Vertex::new(id, "person"), GRAPH);
    }
    g.add_edge(Edge::new(10, 1, 2, "knows"), GRAPH);
    g.add_edge(Edge::new(11, 2, 3, "knows"), GRAPH);
    g.add_edge(Edge::new(12, 1, 3, "knows"), GRAPH);
    g.add_edge(Edge::new(13, 1, 4, "knows"), GRAPH);
    g
}

/// Build a 2-vertex 1-edge pattern with the given variable names.
fn two_hop_pattern(src: &str, dst: &str) -> GraphPattern {
    GraphPattern::new()
        .add_vertex(VertexPattern::new(src))
        .add_vertex(VertexPattern::new(dst))
        .add_edge(EdgePattern::new(src, dst).with_label("knows"))
}

/// Build a 3-vertex 2-edge chain pattern (a -> b -> c).
fn three_hop_chain(a: &str, b: &str, c: &str) -> GraphPattern {
    GraphPattern::new()
        .add_vertex(VertexPattern::new(a))
        .add_vertex(VertexPattern::new(b))
        .add_vertex(VertexPattern::new(c))
        .add_edge(EdgePattern::new(a, b).with_label("knows"))
        .add_edge(EdgePattern::new(b, c).with_label("knows"))
}

/// Pull each match's `subgraph_vertices` (as a sorted set) out of a
/// `GraphPostingList`. The match identity is the *set* of vertices
/// it covers; variable names are not part of that identity.
fn match_signatures(g: &uqa_graph::GraphPostingList) -> BTreeSet<Vec<VertexId>> {
    let mut sigs: BTreeSet<Vec<VertexId>> = BTreeSet::new();
    for entry in g.inner() {
        let payload = g
            .get_graph_payload(entry.doc_id)
            .cloned()
            .unwrap_or_default();
        let mut vs = payload.subgraph_vertices.clone();
        vs.sort_unstable();
        vs.dedup();
        sigs.insert(vs);
    }
    sigs
}

const ALPHABET: &[&str] = &["a", "b", "c", "d", "x", "y", "z"];

/// Strategy: pick three distinct variable names from a small alphabet.
fn arb_three_names() -> impl Strategy<Value = (String, String, String)> {
    let n = ALPHABET.len();
    (0usize..n, 0usize..n, 0usize..n).prop_filter_map("distinct", move |(i, j, k)| {
        if i == j || j == k || i == k {
            None
        } else {
            Some((ALPHABET[i].into(), ALPHABET[j].into(), ALPHABET[k].into()))
        }
    })
}

/// Strategy: two distinct variable names.
fn arb_two_names() -> impl Strategy<Value = (String, String)> {
    let n = ALPHABET.len();
    (0usize..n, 0usize..n).prop_filter_map("distinct", move |(i, j)| {
        if i == j {
            None
        } else {
            Some((ALPHABET[i].into(), ALPHABET[j].into()))
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// Renaming `(src, dst)` in a 2-vertex 1-edge pattern preserves
    /// the matched subgraph signatures.
    #[test]
    fn two_hop_rename_equivalence(
        (a1, b1) in arb_two_names(),
        (a2, b2) in arb_two_names(),
    ) {
        let store = corpus();
        let p1 = two_hop_pattern(&a1, &b1);
        let p2 = two_hop_pattern(&a2, &b2);

        let r1 = GMatch::new(p1, GRAPH).execute(&store);
        let r2 = GMatch::new(p2, GRAPH).execute(&store);

        prop_assert_eq!(match_signatures(&r1), match_signatures(&r2));
    }

    /// Renaming `(a, b, c)` in a 3-vertex 2-edge chain pattern
    /// preserves the matched subgraph signatures.
    #[test]
    fn three_hop_chain_rename_equivalence(
        (a1, b1, c1) in arb_three_names(),
        (a2, b2, c2) in arb_three_names(),
    ) {
        let store = corpus();
        let p1 = three_hop_chain(&a1, &b1, &c1);
        let p2 = three_hop_chain(&a2, &b2, &c2);

        let r1 = GMatch::new(p1, GRAPH).execute(&store);
        let r2 = GMatch::new(p2, GRAPH).execute(&store);

        prop_assert_eq!(match_signatures(&r1), match_signatures(&r2));
    }

    /// Specifically: renaming via a permutation map preserves count.
    /// Even if some variables happen to coincide between P1 and P2,
    /// the *count* of distinct subgraph signatures is invariant
    /// because they're keyed on vertex sets, not names.
    #[test]
    fn rename_preserves_match_count(
        (a, b, c) in arb_three_names(),
    ) {
        let store = corpus();
        let baseline = three_hop_chain("a", "b", "c");
        let renamed = three_hop_chain(&a, &b, &c);

        let r_baseline = GMatch::new(baseline, GRAPH).execute(&store);
        let r_renamed = GMatch::new(renamed, GRAPH).execute(&store);

        let sigs_baseline = match_signatures(&r_baseline);
        let sigs_renamed = match_signatures(&r_renamed);
        prop_assert_eq!(sigs_baseline.len(), sigs_renamed.len());
    }
}

/// Reference: the baseline `(a, b)` 2-hop pattern produces a
/// non-empty match set. Sanity check so the property tests aren't
/// vacuously passing on empty results.
#[test]
fn baseline_two_hop_is_non_empty() {
    let store = corpus();
    let pattern = two_hop_pattern("a", "b");
    let r = GMatch::new(pattern, GRAPH).execute(&store);
    let sigs = match_signatures(&r);
    assert!(!sigs.is_empty(), "expected non-empty matches, got {sigs:?}");
}
