//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Posting list and its Boolean algebra.
//!
//! `PostingList` is an ordered sequence of `(doc_id, payload)` pairs sorted
//! ascending by `doc_id` with no duplicate `doc_id`. The structure
//! `(L, union, intersect, complement, empty, universal)` is a complete
//! Boolean algebra (Theorem 2.1.2, Paper 1).
//!
//! # Equality semantics
//!
//! Derived `PartialEq` on `PostingList` compares full entries (doc id +
//! payload). The Boolean algebra identities (idempotence, distributivity,
//! De Morgan, ...) hold over the *doc id set*, not over scores: [`union`]
//! and [`intersect`] additively merge payloads (positions union, scores
//! added, fields right-wins on key collision). Tests that assert algebraic
//! equalities must compare via [`PostingList::doc_id_set`] or
//! [`PostingList::doc_ids_eq`].
//!
//! [`union`]: PostingList::union
//! [`intersect`]: PostingList::intersect

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{BitAnd, BitOr, Sub};

use crate::types::{
    DocId, GeneralizedPostingEntry, GraphPhiEnvelope, GraphPhiPayload, Payload, PostingEntry,
    Value, GRAPH_PHI_EDGES_FIELD, GRAPH_PHI_FIELD, GRAPH_PHI_VERTICES_FIELD,
};

/// Ordered sequence of `(doc_id, payload)` pairs.
///
/// Invariant: `entries` is sorted by `doc_id` ascending and contains no
/// duplicate `doc_id`s.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostingList {
    entries: Vec<PostingEntry>,
}

// Pinned by `posting_list_intersect_consuming_inputs`: below this input size a small result allocation is cheaper than in-place compaction, while larger lists benefit from reusing the left buffer.
const INTERSECT_REUSE_MIN_ENTRIES: usize = 4_096;

impl PostingList {
    /// Construct an empty posting list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct from possibly unsorted, possibly duplicated entries.
    ///
    /// Sorts by `doc_id` ascending and keeps the first occurrence on
    /// duplicate `doc_id`s.
    pub fn from_unsorted(mut entries: Vec<PostingEntry>) -> Self {
        entries.sort_by_key(|e| e.doc_id);
        entries.dedup_by_key(|e| e.doc_id);
        Self { entries }
    }

    /// Construct from entries that are already sorted by `doc_id` ascending
    /// and contain no duplicate `doc_id`s.
    ///
    /// In debug builds this checks the invariant. In release builds it is
    /// O(1) — the caller is responsible for upholding the invariant. Used
    /// by internal merges that produce sorted output by construction.
    pub fn from_sorted_unchecked(entries: Vec<PostingEntry>) -> Self {
        debug_assert!(
            entries.windows(2).all(|w| w[0].doc_id < w[1].doc_id),
            "PostingList::from_sorted_unchecked invariant violated"
        );
        Self { entries }
    }

    /// `A union B`: keep all `doc_id`s from either side, merging payloads on
    /// collision.
    pub fn union(&self, other: &Self) -> Self {
        let (a, b) = (&self.entries, &other.entries);
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].doc_id.cmp(&b[j].doc_id) {
                std::cmp::Ordering::Equal => {
                    out.push(PostingEntry {
                        doc_id: a[i].doc_id,
                        payload: merge_payloads(&a[i].payload, &b[j].payload),
                    });
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => {
                    out.push(a[i].clone());
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    out.push(b[j].clone());
                    j += 1;
                }
            }
        }
        out.extend_from_slice(&a[i..]);
        out.extend_from_slice(&b[j..]);
        Self::from_sorted_unchecked(out)
    }

    /// `A intersect B`: keep only `doc_id`s present in both sides, merging
    /// payloads.
    pub fn intersect(&self, other: &Self) -> Self {
        let (a, b) = (&self.entries, &other.entries);
        let mut out = Vec::with_capacity(a.len().min(b.len()));
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].doc_id.cmp(&b[j].doc_id) {
                std::cmp::Ordering::Equal => {
                    out.push(PostingEntry {
                        doc_id: a[i].doc_id,
                        payload: merge_payloads(&a[i].payload, &b[j].payload),
                    });
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
            }
        }
        Self::from_sorted_unchecked(out)
    }

    /// Consuming intersection that avoids a result allocation for large inputs by reusing the left posting buffer. Small inputs retain the lower-overhead allocating path. Payload semantics are identical to [`PostingList::intersect`], including right-hand field precedence.
    #[inline]
    pub fn intersect_owned(self, other: &Self) -> Self {
        if self.entries.len().min(other.entries.len()) < INTERSECT_REUSE_MIN_ENTRIES {
            return self.intersect(other);
        }
        self.intersect_reusing_left(other)
    }

    #[inline(never)]
    fn intersect_reusing_left(mut self, other: &Self) -> Self {
        let mut other_index = 0;
        self.entries.retain_mut(|entry| {
            while other_index < other.entries.len()
                && other.entries[other_index].doc_id < entry.doc_id
            {
                other_index += 1;
            }
            if other_index >= other.entries.len()
                || other.entries[other_index].doc_id != entry.doc_id
            {
                return false;
            }
            entry.payload = merge_payloads(&entry.payload, &other.entries[other_index].payload);
            other_index += 1;
            true
        });
        self
    }

    /// `A - B`: entries of `A` whose `doc_id` does not appear in `B`.
    ///
    /// Set-membership filter rather than a two-pointer loop: for sparse `B`
    /// the hash lookup amortizes better than skip-and-compare, and the
    /// output preserves the sorted invariant trivially.
    pub fn difference(&self, other: &Self) -> Self {
        let other_ids: BTreeSet<DocId> = other.entries.iter().map(|e| e.doc_id).collect();
        let out: Vec<PostingEntry> = self
            .entries
            .iter()
            .filter(|e| !other_ids.contains(&e.doc_id))
            .cloned()
            .collect();
        Self::from_sorted_unchecked(out)
    }

    /// Complement of `self` with respect to a universal set `universal`:
    /// `universal - self`.
    pub fn complement(&self, universal: &Self) -> Self {
        universal.difference(self)
    }

    /// Top-`k` entries by `payload.score` (descending). Ties broken by
    /// ascending `doc_id`. Output is re-sorted by `doc_id` so the
    /// invariant is preserved.
    pub fn top_k(&self, k: usize) -> Self {
        if k >= self.entries.len() {
            return self.clone();
        }
        let mut scored: Vec<&PostingEntry> = self.entries.iter().collect();
        scored.sort_by(|a, b| {
            b.payload
                .score
                .total_cmp(&a.payload.score)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        let mut top: Vec<PostingEntry> = scored.into_iter().take(k).cloned().collect();
        top.sort_by_key(|e| e.doc_id);
        Self::from_sorted_unchecked(top)
    }

    /// Apply a scoring function to every entry, returning a new posting list.
    pub fn with_scores<F>(&self, score_fn: F) -> Self
    where
        F: Fn(&PostingEntry) -> f64,
    {
        let entries = self
            .entries
            .iter()
            .map(|e| PostingEntry {
                doc_id: e.doc_id,
                payload: Payload {
                    positions: e.payload.positions.clone(),
                    score: score_fn(e),
                    fields: e.payload.fields.clone(),
                },
            })
            .collect();
        Self::from_sorted_unchecked(entries)
    }

    /// Look up an entry by `doc_id`. O(log n) via binary search.
    pub fn get_entry(&self, doc_id: DocId) -> Option<&PostingEntry> {
        self.entries
            .binary_search_by_key(&doc_id, |e| e.doc_id)
            .ok()
            .map(|i| &self.entries[i])
    }

    pub fn entries(&self) -> &[PostingEntry] {
        &self.entries
    }

    pub fn doc_ids(&self) -> impl Iterator<Item = DocId> + '_ {
        self.entries.iter().map(|e| e.doc_id)
    }

    /// `BTreeSet` of doc ids — convenient for set-level equality checks in
    /// property tests.
    pub fn doc_id_set(&self) -> BTreeSet<DocId> {
        self.entries.iter().map(|e| e.doc_id).collect()
    }

    /// Compare by doc-id sequence only, ignoring payloads. This is the
    /// equality the Boolean algebra identities are stated over.
    pub fn doc_ids_eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .zip(other.entries.iter())
                .all(|(a, b)| a.doc_id == b.doc_id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, PostingEntry> {
        self.entries.iter()
    }
}

impl IntoIterator for PostingList {
    type Item = PostingEntry;
    type IntoIter = std::vec::IntoIter<PostingEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a> IntoIterator for &'a PostingList {
    type Item = &'a PostingEntry;
    type IntoIter = std::slice::Iter<'a, PostingEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl BitOr for &PostingList {
    type Output = PostingList;
    fn bitor(self, rhs: Self) -> PostingList {
        self.union(rhs)
    }
}

impl BitAnd for &PostingList {
    type Output = PostingList;
    fn bitand(self, rhs: Self) -> PostingList {
        self.intersect(rhs)
    }
}

impl Sub for &PostingList {
    type Output = PostingList;
    fn sub(self, rhs: Self) -> PostingList {
        self.difference(rhs)
    }
}

impl FromIterator<PostingEntry> for PostingList {
    fn from_iter<I: IntoIterator<Item = PostingEntry>>(iter: I) -> Self {
        Self::from_unsorted(iter.into_iter().collect())
    }
}

/// Merge two payloads when their parent entries collide on `doc_id`.
///
/// - positions: union, sorted ascending, deduped
/// - score: added (so `union(a, a)` doubles the score; algebraic identities
///   are stated over doc-id sets, not scores)
/// - fields: right-hand side wins on key collision
fn merge_payloads(a: &Payload, b: &Payload) -> Payload {
    let a_phi = GraphPhiEnvelope::decode(a.fields.get(GRAPH_PHI_FIELD));
    let b_phi = GraphPhiEnvelope::decode(b.fields.get(GRAPH_PHI_FIELD));
    if a_phi.is_some() || b_phi.is_some() {
        return merge_phi_payloads(a, b, a_phi, b_phi);
    }

    let a_score_only = a.positions.is_empty() && a.fields.is_empty();
    let b_score_only = b.positions.is_empty() && b.fields.is_empty();
    if a_score_only && b_score_only {
        return Payload::with_score(a.score + b.score);
    }
    if a_score_only {
        let mut merged = b.clone();
        merged.score = a.score + b.score;
        return merged;
    }
    if b_score_only {
        let mut merged = a.clone();
        merged.score = a.score + b.score;
        return merged;
    }

    let mut positions: Vec<u32> = Vec::with_capacity(a.positions.len() + b.positions.len());
    positions.extend_from_slice(&a.positions);
    positions.extend_from_slice(&b.positions);
    positions.sort_unstable();
    positions.dedup();

    let mut fields: BTreeMap<String, Value> = a.fields.clone();
    for (k, v) in &b.fields {
        fields.insert(k.clone(), v.clone());
    }

    Payload {
        positions,
        score: a.score + b.score,
        fields,
    }
}

fn merge_phi_payloads(
    a: &Payload,
    b: &Payload,
    a_phi: Option<GraphPhiEnvelope>,
    b_phi: Option<GraphPhiEnvelope>,
) -> Payload {
    let (a_base_score, a_graph, a_override, a_fields) = decode_phi_payload(a, a_phi);
    let (b_base_score, b_graph, b_override, b_fields) = decode_phi_payload(b, b_phi);

    let mut positions: Vec<u32> = Vec::with_capacity(a.positions.len() + b.positions.len());
    positions.extend_from_slice(&a.positions);
    positions.extend_from_slice(&b.positions);
    positions.sort_unstable();
    positions.dedup();

    let mut fields = a_fields;
    fields.extend(b_fields);
    let original_reserved = fields.remove(GRAPH_PHI_FIELD);
    let original_vertices = fields.remove(GRAPH_PHI_VERTICES_FIELD);
    let original_edges = fields.remove(GRAPH_PHI_EDGES_FIELD);

    let graph_payload = b_graph.or(a_graph);
    let merged_score = a.score + b.score;
    let score_override =
        if graph_payload.is_some() && (a_override.is_some() || b_override.is_some()) {
            Some(merged_score)
        } else {
            None
        };

    if let Some(graph) = &graph_payload {
        fields.insert(
            GRAPH_PHI_VERTICES_FIELD.to_string(),
            graph.encoded_vertices(),
        );
        fields.insert(GRAPH_PHI_EDGES_FIELD.to_string(), graph.encoded_edges());
    } else {
        restore_field(
            &mut fields,
            GRAPH_PHI_VERTICES_FIELD,
            original_vertices.clone(),
        );
        restore_field(&mut fields, GRAPH_PHI_EDGES_FIELD, original_edges.clone());
    }

    fields.insert(
        GRAPH_PHI_FIELD.to_string(),
        GraphPhiEnvelope {
            base_score: a_base_score + b_base_score,
            graph_payload,
            score_override,
            original_reserved,
            original_vertices,
            original_edges,
        }
        .encode(),
    );

    Payload {
        positions,
        score: merged_score,
        fields,
    }
}

fn decode_phi_payload(
    payload: &Payload,
    envelope: Option<GraphPhiEnvelope>,
) -> (
    f64,
    Option<GraphPhiPayload>,
    Option<f64>,
    BTreeMap<String, Value>,
) {
    let Some(envelope) = envelope else {
        return (payload.score, None, None, payload.fields.clone());
    };

    let mut fields = payload.fields.clone();
    fields.remove(GRAPH_PHI_FIELD);
    fields.remove(GRAPH_PHI_VERTICES_FIELD);
    fields.remove(GRAPH_PHI_EDGES_FIELD);
    restore_field(&mut fields, GRAPH_PHI_FIELD, envelope.original_reserved);
    restore_field(
        &mut fields,
        GRAPH_PHI_VERTICES_FIELD,
        envelope.original_vertices,
    );
    restore_field(&mut fields, GRAPH_PHI_EDGES_FIELD, envelope.original_edges);
    (
        envelope.base_score,
        envelope.graph_payload,
        envelope.score_override,
        fields,
    )
}

fn restore_field(fields: &mut BTreeMap<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        fields.insert(key.to_string(), value);
    }
}

/// Posting list with multi-document tuples, the carrier for join results
/// (Definition 4.1.2, Paper 1).
///
/// Invariant: `entries` sorted by the `doc_ids` tuple, no duplicates.
#[derive(Debug, Clone, Default)]
pub struct GeneralizedPostingList {
    entries: Vec<GeneralizedPostingEntry>,
}

impl GeneralizedPostingList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_unsorted(mut entries: Vec<GeneralizedPostingEntry>) -> Self {
        entries.sort();
        entries.dedup_by(|a, b| a.doc_ids == b.doc_ids);
        Self { entries }
    }

    pub fn from_sorted_unchecked(entries: Vec<GeneralizedPostingEntry>) -> Self {
        debug_assert!(
            entries.windows(2).all(|w| w[0].doc_ids < w[1].doc_ids),
            "GeneralizedPostingList::from_sorted_unchecked invariant violated"
        );
        Self { entries }
    }

    pub fn union(&self, other: &Self) -> Self {
        let (a, b) = (&self.entries, &other.entries);
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].doc_ids.cmp(&b[j].doc_ids) {
                std::cmp::Ordering::Equal => {
                    out.push(a[i].clone());
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => {
                    out.push(a[i].clone());
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    out.push(b[j].clone());
                    j += 1;
                }
            }
        }
        out.extend_from_slice(&a[i..]);
        out.extend_from_slice(&b[j..]);
        Self::from_sorted_unchecked(out)
    }

    pub fn intersect(&self, other: &Self) -> Self {
        let (a, b) = (&self.entries, &other.entries);
        let mut out = Vec::with_capacity(a.len().min(b.len()));
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].doc_ids.cmp(&b[j].doc_ids) {
                std::cmp::Ordering::Equal => {
                    out.push(a[i].clone());
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
            }
        }
        Self::from_sorted_unchecked(out)
    }

    pub fn difference(&self, other: &Self) -> Self {
        let other_ids: BTreeSet<&Vec<DocId>> = other.entries.iter().map(|e| &e.doc_ids).collect();
        let out: Vec<GeneralizedPostingEntry> = self
            .entries
            .iter()
            .filter(|e| !other_ids.contains(&e.doc_ids))
            .cloned()
            .collect();
        Self::from_sorted_unchecked(out)
    }

    pub fn complement(&self, universal: &Self) -> Self {
        universal.difference(self)
    }

    pub fn entries(&self) -> &[GeneralizedPostingEntry] {
        &self.entries
    }

    pub fn doc_ids_set(&self) -> BTreeSet<Vec<DocId>> {
        self.entries.iter().map(|e| e.doc_ids.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl PartialEq for GeneralizedPostingList {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .zip(other.entries.iter())
                .all(|(a, b)| a.doc_ids == b.doc_ids)
    }
}

impl Eq for GeneralizedPostingList {}

impl FromIterator<GeneralizedPostingEntry> for GeneralizedPostingList {
    fn from_iter<I: IntoIterator<Item = GeneralizedPostingEntry>>(iter: I) -> Self {
        Self::from_unsorted(iter.into_iter().collect())
    }
}

impl BitOr for &GeneralizedPostingList {
    type Output = GeneralizedPostingList;
    fn bitor(self, rhs: Self) -> GeneralizedPostingList {
        self.union(rhs)
    }
}

impl BitAnd for &GeneralizedPostingList {
    type Output = GeneralizedPostingList;
    fn bitand(self, rhs: Self) -> GeneralizedPostingList {
        self.intersect(rhs)
    }
}

impl Sub for &GeneralizedPostingList {
    type Output = GeneralizedPostingList;
    fn sub(self, rhs: Self) -> GeneralizedPostingList {
        self.difference(rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GeneralizedPayload, Payload};

    fn pl_of(ids: &[DocId]) -> PostingList {
        PostingList::from_unsorted(
            ids.iter()
                .map(|id| PostingEntry::new(*id, Payload::default()))
                .collect(),
        )
    }

    fn ids(pl: &PostingList) -> Vec<DocId> {
        pl.doc_ids().collect()
    }

    #[test]
    fn empty_list_is_empty() {
        let pl = PostingList::new();
        assert!(pl.is_empty());
        assert_eq!(pl.len(), 0);
    }

    #[test]
    fn from_unsorted_sorts_and_dedups() {
        let pl = pl_of(&[3, 1, 2, 1]);
        assert_eq!(ids(&pl), vec![1, 2, 3]);
    }

    #[test]
    fn union_merges_two_pointer() {
        let a = pl_of(&[1, 3, 5]);
        let b = pl_of(&[2, 3, 4]);
        assert_eq!(ids(&a.union(&b)), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn intersect_keeps_common() {
        let a = pl_of(&[1, 3, 5]);
        let b = pl_of(&[2, 3, 4, 5]);
        assert_eq!(ids(&a.intersect(&b)), vec![3, 5]);
    }

    #[test]
    fn difference_excludes_other_ids() {
        let a = pl_of(&[1, 2, 3, 4]);
        let b = pl_of(&[2, 4]);
        assert_eq!(ids(&a.difference(&b)), vec![1, 3]);
    }

    #[test]
    fn complement_uses_universal() {
        let a = pl_of(&[2, 4]);
        let universal = pl_of(&[1, 2, 3, 4, 5]);
        assert_eq!(ids(&a.complement(&universal)), vec![1, 3, 5]);
    }

    #[test]
    fn operator_overloads() {
        let a = pl_of(&[1, 2, 3]);
        let b = pl_of(&[2, 3, 4]);
        assert_eq!(ids(&(&a | &b)), vec![1, 2, 3, 4]);
        assert_eq!(ids(&(&a & &b)), vec![2, 3]);
        assert_eq!(ids(&(&a - &b)), vec![1]);
    }

    #[test]
    fn top_k_keeps_highest_scores() {
        let entries = vec![
            PostingEntry::new(1, Payload::with_score(0.1)),
            PostingEntry::new(2, Payload::with_score(0.9)),
            PostingEntry::new(3, Payload::with_score(0.5)),
            PostingEntry::new(4, Payload::with_score(0.7)),
        ];
        let pl = PostingList::from_unsorted(entries);
        let top2 = pl.top_k(2);
        assert_eq!(ids(&top2), vec![2, 4]); // re-sorted by doc_id ascending
    }

    #[test]
    fn get_entry_uses_binary_search() {
        let pl = pl_of(&[10, 20, 30, 40, 50]);
        assert!(pl.get_entry(30).is_some());
        assert!(pl.get_entry(35).is_none());
    }

    #[test]
    fn merge_payloads_combines_positions_and_scores() {
        let a = Payload {
            positions: vec![1, 3],
            score: 1.0,
            ..Payload::default()
        };
        let b = Payload {
            positions: vec![2, 3],
            score: 2.5,
            ..Payload::default()
        };
        let merged = merge_payloads(&a, &b);
        assert_eq!(merged.positions, vec![1, 2, 3]);
        assert!((merged.score - 3.5).abs() < f64::EPSILON);
    }

    #[test]
    fn consuming_intersection_matches_borrowed_payload_semantics() {
        let left = PostingList::from_sorted_unchecked(vec![
            PostingEntry::new(1, Payload::with_score(1.0)),
            PostingEntry::new(
                3,
                Payload {
                    positions: vec![1, 4],
                    score: 2.0,
                    fields: BTreeMap::from([
                        ("left".into(), Value::Bool(true)),
                        ("shared".into(), Value::Str("left".into())),
                    ]),
                },
            ),
            PostingEntry::new(5, Payload::default()),
        ]);
        let right = PostingList::from_sorted_unchecked(vec![
            PostingEntry::new(2, Payload::default()),
            PostingEntry::new(
                3,
                Payload {
                    positions: vec![2, 4],
                    score: 4.0,
                    fields: BTreeMap::from([
                        ("right".into(), Value::Bool(true)),
                        ("shared".into(), Value::Str("right".into())),
                    ]),
                },
            ),
            PostingEntry::new(5, Payload::with_score(8.0)),
        ]);

        assert_eq!(left.clone().intersect_owned(&right), left.intersect(&right));
    }

    #[test]
    fn doc_ids_eq_ignores_payloads() {
        let a = PostingList::from_unsorted(vec![PostingEntry::new(1, Payload::with_score(1.0))]);
        let b = PostingList::from_unsorted(vec![PostingEntry::new(1, Payload::with_score(99.0))]);
        assert!(a.doc_ids_eq(&b));
    }

    #[test]
    fn generalized_list_lex_orders_tuples() {
        let mk = |t: Vec<DocId>| GeneralizedPostingEntry {
            doc_ids: t,
            payload: GeneralizedPayload::default(),
        };
        let gpl = GeneralizedPostingList::from_unsorted(vec![
            mk(vec![2, 1]),
            mk(vec![1, 1]),
            mk(vec![1, 2]),
        ]);
        let want: Vec<Vec<DocId>> = gpl.entries().iter().map(|e| e.doc_ids.clone()).collect();
        assert_eq!(want, vec![vec![1, 1], vec![1, 2], vec![2, 1]]);
    }

    #[test]
    fn generalized_intersect_two_pointer() {
        let mk = |t: Vec<DocId>| GeneralizedPostingEntry {
            doc_ids: t,
            payload: GeneralizedPayload::default(),
        };
        let a = GeneralizedPostingList::from_unsorted(vec![
            mk(vec![1, 1]),
            mk(vec![1, 2]),
            mk(vec![2, 3]),
        ]);
        let b = GeneralizedPostingList::from_unsorted(vec![mk(vec![1, 2]), mk(vec![2, 3])]);
        let inter = a.intersect(&b);
        let want: Vec<Vec<DocId>> = inter.entries().iter().map(|e| e.doc_ids.clone()).collect();
        assert_eq!(want, vec![vec![1, 2], vec![2, 3]]);
    }
}
