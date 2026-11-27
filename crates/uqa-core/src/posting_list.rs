//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Payload-bearing posting storage and merge operations.
//!
//! `PostingList` is an ordered sequence of `(doc_id, payload)` pairs sorted
//! ascending by `doc_id` with no duplicate `doc_id`. Its document support is
//! projected explicitly as [`DocSet`], which is the carrier of the Boolean
//! algebra.
//!
//! # Equality semantics
//!
//! Derived `PartialEq` on `PostingList` compares full entries (doc id +
//! payload). [`PostingList::merge_union`] and
//! [`PostingList::merge_intersection`] select union/intersection support while
//! applying a payload merge policy: positions are unioned, scores are added,
//! and fields use the right-hand value on collision. Consequently those
//! operations are generally neither idempotent nor commutative under full
//! `PostingList` equality.
//!
//! Projecting a posting list to [`DocSet`] loses payloads. Reconstructing a
//! posting list from that support therefore creates default payloads and is
//! not equal to the original decorated posting list in general.

use std::cmp::Reverse;
use std::collections::{binary_heap::PeekMut, BTreeMap, BTreeSet, BinaryHeap};

use crate::types::{
    DocId, GeneralizedPostingEntry, GraphPhiEnvelope, GraphPhiPayload, Payload, PostingEntry,
    Value, GRAPH_PHI_EDGES_FIELD, GRAPH_PHI_FIELD, GRAPH_PHI_VERTICES_FIELD,
};
use crate::{DocSet, RankedView};

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

    /// Keep the union of both supports and merge payloads on a shared
    /// `doc_id`.
    ///
    /// This is not Boolean join on full posting values: scores add and fields
    /// from `other` take precedence on collision. Use [`Self::support`] when a
    /// Boolean-algebra value is required.
    pub fn merge_union(&self, other: &Self) -> Self {
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

    /// Merge sorted lists in a single pass. Shared documents fold their
    /// payloads in input order, exactly as successive binary unions would.
    pub fn merge_union_all(mut lists: Vec<Self>) -> Self {
        lists.retain(|list| !list.is_empty());
        if lists.len() <= 1 {
            return lists.pop().unwrap_or_default();
        }
        let mut entries: Vec<PostingEntry> =
            Vec::with_capacity(lists.iter().map(Self::len).max().unwrap_or(0));
        let mut inputs: Vec<_> = lists.into_iter().map(IntoIterator::into_iter).collect();
        let mut heads: BinaryHeap<_> = inputs
            .iter()
            .enumerate()
            .map(|(source, input)| Reverse((input.as_slice()[0].doc_id, source)))
            .collect();
        while let Some(mut head) = heads.peek_mut() {
            let Reverse((doc_id, source)) = *head;
            let input = &mut inputs[source];
            let entry = input.next().expect("heap contains only nonempty lists");
            if let Some(next) = input.as_slice().first() {
                *head = Reverse((next.doc_id, source));
            } else {
                PeekMut::pop(head);
            }
            if let Some(previous) = entries.last_mut().filter(|entry| entry.doc_id == doc_id) {
                previous.payload = merge_payloads(&previous.payload, &entry.payload);
            } else {
                entries.push(entry);
            }
        }
        Self::from_sorted_unchecked(entries)
    }

    /// Keep the intersection of both supports and merge payloads on each
    /// shared `doc_id`.
    ///
    /// This is not Boolean meet on full posting values. Its payload policy is
    /// the same as [`Self::merge_union`].
    pub fn merge_intersection(&self, other: &Self) -> Self {
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

    /// Consuming intersection merge that avoids a result allocation for large
    /// inputs by reusing the left posting buffer. Small inputs retain the
    /// lower-overhead allocating path. Payload semantics are identical to
    /// [`PostingList::merge_intersection`], including right-hand field
    /// precedence.
    #[inline]
    pub fn merge_intersection_owned(self, other: &Self) -> Self {
        if self.entries.len().min(other.entries.len()) < INTERSECT_REUSE_MIN_ENTRIES {
            return self.merge_intersection(other);
        }
        self.merge_intersection_reusing_left(other)
    }

    /// Intersect document support and reconstruct every retained entry with a
    /// default payload. This is the physical meet for membership-only operator
    /// subtrees; unlike [`Self::merge_intersection_owned`], it deliberately
    /// does not observe or combine scores, positions, or fields.
    pub fn merge_support_intersection_owned(mut self, other: &Self) -> Self {
        let mut lookup = other.lookup_ascending();
        self.entries.retain_mut(|entry| {
            if lookup(entry.doc_id).is_none() {
                return false;
            }
            entry.payload = Payload::default();
            true
        });
        self
    }

    #[inline(never)]
    fn merge_intersection_reusing_left(mut self, other: &Self) -> Self {
        let mut lookup = other.lookup_ascending();
        self.entries.retain_mut(|entry| {
            let Some(right) = lookup(entry.doc_id) else {
                return false;
            };
            entry.payload = merge_payloads(&entry.payload, &right.payload);
            true
        });
        self
    }

    /// `A - B`: entries of `A` whose `doc_id` does not appear in `B`.
    ///
    /// Walk both sorted inputs once, preserving the left payloads.
    pub fn exclude(&self, other: &Self) -> Self {
        let mut lookup = other.lookup_ascending();
        let entries = self
            .entries
            .iter()
            .filter(|entry| lookup(entry.doc_id).is_none())
            .cloned()
            .collect();
        Self::from_sorted_unchecked(entries)
    }

    /// Build a score-ordered view without changing posting storage order.
    pub fn ranked(&self) -> RankedView<'_> {
        RankedView::new(self)
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

    /// Look up a nondecreasing sequence of document IDs in a single forward
    /// pass. A repeated ID returns the same entry.
    pub fn lookup_ascending<'a>(&'a self) -> impl FnMut(DocId) -> Option<&'a PostingEntry> {
        let mut entries = self.iter().peekable();
        move |id| {
            while entries.next_if(|entry| entry.doc_id < id).is_some() {}
            entries.peek().copied().filter(|entry| entry.doc_id == id)
        }
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

    /// Project this payload-bearing relation onto its document-id support.
    pub fn support(&self) -> DocSet {
        DocSet::from_sorted_unchecked(self.doc_ids().collect())
    }

    /// Create document-id-ordered posting storage with a default payload for
    /// every id in `support`.
    pub fn from_support(support: &DocSet) -> Self {
        let entries = support
            .iter()
            .map(|doc_id| PostingEntry::new(doc_id, Payload::default()))
            .collect();
        Self::from_sorted_unchecked(entries)
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

impl FromIterator<PostingEntry> for PostingList {
    fn from_iter<I: IntoIterator<Item = PostingEntry>>(iter: I) -> Self {
        Self::from_unsorted(iter.into_iter().collect())
    }
}

impl From<&DocSet> for PostingList {
    fn from(support: &DocSet) -> Self {
        Self::from_support(support)
    }
}

impl From<DocSet> for PostingList {
    fn from(support: DocSet) -> Self {
        Self::from_support(&support)
    }
}

impl From<&PostingList> for DocSet {
    fn from(posting_list: &PostingList) -> Self {
        posting_list.support()
    }
}

/// Merge two payloads when their parent entries collide on `doc_id`.
///
/// - positions: union, sorted ascending, deduped
/// - score: added (so `merge_union(a, a)` generally differs from `a`)
/// - fields: right-hand side wins on key collision
fn merge_payloads(a: &Payload, b: &Payload) -> Payload {
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

    let a_phi = GraphPhiEnvelope::decode(a.fields.get(GRAPH_PHI_FIELD));
    let b_phi = GraphPhiEnvelope::decode(b.fields.get(GRAPH_PHI_FIELD));
    if a_phi.is_some() || b_phi.is_some() {
        return merge_phi_payloads(a, b, a_phi, b_phi);
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

/// Payload-bearing, multi-document tuple storage for join results.
///
/// Invariant: `entries` sorted by the `doc_ids` tuple, no duplicates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

    /// Select the union of tuple support, preserving the left payload when a
    /// tuple occurs on both sides.
    pub fn merge_union(&self, other: &Self) -> Self {
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

    /// Select the intersection of tuple support, preserving left payloads.
    pub fn merge_intersection(&self, other: &Self) -> Self {
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

    /// Preserve left entries whose tuple support is absent from `other`.
    pub fn exclude(&self, other: &Self) -> Self {
        let other_ids: BTreeSet<&Vec<DocId>> = other.entries.iter().map(|e| &e.doc_ids).collect();
        let out: Vec<GeneralizedPostingEntry> = self
            .entries
            .iter()
            .filter(|e| !other_ids.contains(&e.doc_ids))
            .cloned()
            .collect();
        Self::from_sorted_unchecked(out)
    }

    pub fn entries(&self) -> &[GeneralizedPostingEntry] {
        &self.entries
    }

    /// Materialize tuple support without payloads.
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

impl FromIterator<GeneralizedPostingEntry> for GeneralizedPostingList {
    fn from_iter<I: IntoIterator<Item = GeneralizedPostingEntry>>(iter: I) -> Self {
        Self::from_unsorted(iter.into_iter().collect())
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
        assert_eq!(ids(&a.merge_union(&b)), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn intersect_keeps_common() {
        let a = pl_of(&[1, 3, 5]);
        let b = pl_of(&[2, 3, 4, 5]);
        assert_eq!(ids(&a.merge_intersection(&b)), vec![3, 5]);
    }

    #[test]
    fn many_list_union_matches_successive_payload_merges_in_operand_order() {
        let lists: Vec<_> = [1e16, -1e16, 1.0]
            .into_iter()
            .enumerate()
            .map(|(i, score)| {
                let payload = Payload {
                    positions: vec![i as u32, 10],
                    score,
                    fields: BTreeMap::from([("winner".into(), Value::Int(i as i64))]),
                };
                PostingList::from_unsorted(vec![
                    PostingEntry::new(u64::MAX, payload),
                    PostingEntry::new(u64::MAX, Payload::with_score(99.0)),
                    PostingEntry::new(i as u64, Payload::with_score(-0.0)),
                ])
            })
            .collect();
        let expected = lists
            .iter()
            .fold(PostingList::new(), |acc, list| acc.merge_union(list));
        let result = PostingList::merge_union_all(lists);
        assert_eq!(result, expected);
        let shared = &result.get_entry(u64::MAX).unwrap().payload;
        assert_eq!(shared.score.to_bits(), 1.0f64.to_bits());
        assert_eq!(shared.fields["winner"], Value::Int(2));
        assert_eq!(
            result.get_entry(0).unwrap().payload.score.to_bits(),
            (-0.0f64).to_bits()
        );
        assert!(PostingList::merge_union_all(vec![PostingList::new()]).is_empty());
        let nan = f64::from_bits(0x7ff8_0000_0000_1234);
        let singleton = PostingList::from_sorted_unchecked(vec![PostingEntry::new(
            7,
            Payload::with_score(nan),
        )]);
        assert_eq!(
            PostingList::merge_union_all(vec![PostingList::new(), singleton]).entries()[0]
                .payload
                .score
                .to_bits(),
            nan.to_bits()
        );
    }

    #[test]
    fn disjoint_many_list_union_moves_payloads_without_per_entry_allocations() {
        let lists = (0..32)
            .map(|signal| {
                PostingList::from_sorted_unchecked(
                    (0..64)
                        .map(|row| {
                            PostingEntry::new(
                                row * 32 + signal,
                                Payload {
                                    fields: BTreeMap::from([(
                                        "field".into(),
                                        Value::Str("x".repeat(1024)),
                                    )]),
                                    ..Default::default()
                                },
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        let mut result = PostingList::new();
        let allocations =
            allocation_counter::measure(|| result = PostingList::merge_union_all(lists));
        assert!(result.doc_ids().eq(0..2048));
        assert!(allocations.count_total < 32, "{allocations:?}");
        let no_matches =
            allocation_counter::measure(|| assert!(result.exclude(&result).is_empty()));
        assert_eq!(no_matches.count_total, 0);
    }

    #[test]
    fn difference_excludes_other_ids() {
        let a = pl_of(&[1, 2, 3, 4]);
        let b = pl_of(&[2, 4]);
        assert_eq!(ids(&a.exclude(&b)), vec![1, 3]);
    }

    #[test]
    fn complement_uses_universal() {
        let a = pl_of(&[2, 4]);
        let universal = pl_of(&[1, 2, 3, 4, 5]);
        assert_eq!(ids(&universal.exclude(&a)), vec![1, 3, 5]);
    }

    #[test]
    fn payload_operations_are_explicit_methods() {
        let a = pl_of(&[1, 2, 3]);
        let b = pl_of(&[2, 3, 4]);
        assert_eq!(ids(&a.merge_union(&b)), vec![1, 2, 3, 4]);
        assert_eq!(ids(&a.merge_intersection(&b)), vec![2, 3]);
        assert_eq!(ids(&a.exclude(&b)), vec![1]);
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
        let top2 = pl.ranked().select_top_k(2);
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

        assert_eq!(
            left.clone().merge_intersection_owned(&right),
            left.merge_intersection(&right)
        );
    }

    #[test]
    fn support_intersection_discards_payloads() {
        let left = PostingList::from_sorted_unchecked(vec![
            PostingEntry::new(1, Payload::with_score(3.0)),
            PostingEntry::new(2, Payload::with_score(4.0)),
        ]);
        let right = PostingList::from_sorted_unchecked(vec![
            PostingEntry::new(2, Payload::with_score(5.0)),
            PostingEntry::new(3, Payload::with_score(6.0)),
        ]);

        assert_eq!(left.merge_support_intersection_owned(&right), pl_of(&[2]));
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
        let inter = a.merge_intersection(&b);
        let want: Vec<Vec<DocId>> = inter.entries().iter().map(|e| e.doc_ids.clone()).collect();
        assert_eq!(want, vec![vec![1, 2], vec![2, 3]]);
    }
}
