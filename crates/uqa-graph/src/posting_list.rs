//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph extension of [`uqa_core::PostingList`].
//!
//! `GraphPostingList` carries a `(doc_id -> GraphPayload)` side-table on
//! top of a standard posting list. The lossless `Phi` encoding shuttles
//! that side-table through encoded field entries on the underlying
//! [`uqa_core::Payload`]. Graph-result merges use explicit subgraph policies;
//! generic posting-list payload merges remain a separate contract.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::types::{
    GraphPhiEnvelope, GraphPhiPayload, GRAPH_PHI_EDGES_FIELD, GRAPH_PHI_FIELD,
    GRAPH_PHI_VERTICES_FIELD,
};
use uqa_core::{DocId, EdgeId, PostingEntry, PostingList, Value, VertexId};

/// Auxiliary payload for graph posting list entries: the subgraph each
/// matched entry refers to, plus a graph-name tag and a score override.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphPayload {
    pub subgraph_vertices: Vec<VertexId>,
    pub subgraph_edges: Vec<EdgeId>,
    pub graph_name: String,
    /// `Some(score)` overrides the underlying posting list's payload
    /// score during `Phi`. `None` keeps the standard payload score.
    pub score_override: Option<f64>,
}

impl GraphPayload {
    pub fn new() -> Self {
        Self::default()
    }
}

/// How graph metadata is combined when two graph results contain the same
/// document. Ordinary posting payloads still follow
/// [`PostingList::merge_union`] / [`PostingList::merge_intersection`]; this
/// policy applies only to the graph name and subgraph vertex/edge sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubgraphMergePolicy {
    Union,
    Intersection,
    PreferLeft,
    PreferRight,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphPostingListError {
    #[error("graph payload for document {doc_id} is outside the posting-list support")]
    PayloadOutsideSupport { doc_id: DocId },
    #[error(
        "cannot combine graph payloads for document {doc_id}: graph names {left:?} and {right:?} conflict"
    )]
    ConflictingGraphNames {
        doc_id: DocId,
        left: String,
        right: String,
    },
}

pub type GraphPostingListResult<T> = Result<T, GraphPostingListError>;

/// Posting list paired with a `(doc_id -> GraphPayload)` map. The
/// invariants of the underlying `PostingList` (sorted, unique doc ids)
/// hold here as well.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphPostingList {
    inner: PostingList,
    graph_payloads: BTreeMap<DocId, GraphPayload>,
}

impl GraphPostingList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a graph posting list while enforcing that every side-table
    /// key belongs to the underlying posting support.
    pub fn try_from_parts(
        inner: PostingList,
        graph_payloads: BTreeMap<DocId, GraphPayload>,
    ) -> GraphPostingListResult<Self> {
        if let Some(doc_id) = graph_payloads
            .keys()
            .find(|doc_id| inner.get_entry(**doc_id).is_none())
        {
            return Err(GraphPostingListError::PayloadOutsideSupport { doc_id: *doc_id });
        }
        Ok(Self {
            inner,
            graph_payloads,
        })
    }

    /// `Phi`: encode the graph payload map onto the underlying posting
    /// list's payload fields. A versioned envelope preserves the original
    /// score, graph name, exact override state, and any values that already
    /// occupied reserved fields. Vertex and edge fields are also emitted as
    /// legacy mirrors for existing posting-list consumers.
    ///
    /// The result composes with the standard posting payload policies; invert
    /// it with [`Self::from_posting_list`] to recover the graph view.
    pub fn to_posting_list(&self) -> PostingList {
        let mut converted = Vec::with_capacity(self.inner.len());
        for entry in self.inner.entries() {
            let mut payload = entry.payload.clone();
            let graph_payload = self.graph_payloads.get(&entry.doc_id);
            let needs_envelope = graph_payload.is_some()
                || payload.fields.contains_key(GRAPH_PHI_FIELD)
                || payload.fields.contains_key(GRAPH_PHI_VERTICES_FIELD)
                || payload.fields.contains_key(GRAPH_PHI_EDGES_FIELD);
            if needs_envelope {
                let original_reserved = payload.fields.remove(GRAPH_PHI_FIELD);
                let original_vertices = payload.fields.remove(GRAPH_PHI_VERTICES_FIELD);
                let original_edges = payload.fields.remove(GRAPH_PHI_EDGES_FIELD);
                let encoded_graph = graph_payload.map(|gp| GraphPhiPayload {
                    vertices: gp.subgraph_vertices.clone(),
                    edges: gp.subgraph_edges.clone(),
                    graph_name: gp.graph_name.clone(),
                });

                if let Some(graph) = &encoded_graph {
                    payload.fields.insert(
                        GRAPH_PHI_VERTICES_FIELD.to_string(),
                        graph.encoded_vertices(),
                    );
                    payload
                        .fields
                        .insert(GRAPH_PHI_EDGES_FIELD.to_string(), graph.encoded_edges());
                } else {
                    restore_payload_field(
                        &mut payload.fields,
                        GRAPH_PHI_VERTICES_FIELD,
                        original_vertices.clone(),
                    );
                    restore_payload_field(
                        &mut payload.fields,
                        GRAPH_PHI_EDGES_FIELD,
                        original_edges.clone(),
                    );
                }
                if let Some(score) = graph_payload.and_then(|gp| gp.score_override) {
                    payload.score = score;
                }
                payload.fields.insert(
                    GRAPH_PHI_FIELD.to_string(),
                    GraphPhiEnvelope {
                        base_score: entry.payload.score,
                        graph_payload: encoded_graph,
                        score_override: graph_payload.and_then(|gp| gp.score_override),
                        original_reserved,
                        original_vertices,
                        original_edges,
                    }
                    .encode(),
                );
            }
            converted.push(PostingEntry::new(entry.doc_id, payload));
        }
        PostingList::from_sorted_unchecked(converted)
    }

    /// `Phi^{-1}`: decode the versioned envelope and rebuild the
    /// `(doc_id -> GraphPayload)` side-table without losing the original
    /// payload. The legacy two-field representation remains readable, though
    /// values it never stored (graph name, base score, and `None` versus
    /// `Some`) cannot be reconstructed from legacy input.
    pub fn from_posting_list(pl: &PostingList) -> Self {
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        let mut entries = Vec::with_capacity(pl.len());
        for entry in pl.entries() {
            let envelope_value = entry.payload.fields.get(GRAPH_PHI_FIELD);
            if let Some(envelope) = GraphPhiEnvelope::decode(envelope_value) {
                let mut p = entry.payload.clone();
                p.score = envelope.base_score;
                p.fields.remove(GRAPH_PHI_FIELD);
                p.fields.remove(GRAPH_PHI_VERTICES_FIELD);
                p.fields.remove(GRAPH_PHI_EDGES_FIELD);
                restore_payload_field(&mut p.fields, GRAPH_PHI_FIELD, envelope.original_reserved);
                restore_payload_field(
                    &mut p.fields,
                    GRAPH_PHI_VERTICES_FIELD,
                    envelope.original_vertices,
                );
                restore_payload_field(
                    &mut p.fields,
                    GRAPH_PHI_EDGES_FIELD,
                    envelope.original_edges,
                );
                if let Some(graph) = envelope.graph_payload {
                    graph_payloads.insert(
                        entry.doc_id,
                        GraphPayload {
                            subgraph_vertices: graph.vertices,
                            subgraph_edges: graph.edges,
                            graph_name: graph.graph_name,
                            score_override: envelope.score_override,
                        },
                    );
                }
                entries.push(PostingEntry::new(entry.doc_id, p));
                continue;
            }

            if GraphPhiEnvelope::is_recognized(envelope_value) {
                entries.push(entry.clone());
                continue;
            }

            let vertices = decode_id_list(entry.payload.fields.get(GRAPH_PHI_VERTICES_FIELD));
            let edges = decode_id_list(entry.payload.fields.get(GRAPH_PHI_EDGES_FIELD));
            let has_graph_fields = entry.payload.fields.contains_key(GRAPH_PHI_VERTICES_FIELD)
                || entry.payload.fields.contains_key(GRAPH_PHI_EDGES_FIELD);
            let mut payload = entry.payload.clone();
            if has_graph_fields {
                payload.fields.remove(GRAPH_PHI_VERTICES_FIELD);
                payload.fields.remove(GRAPH_PHI_EDGES_FIELD);
                graph_payloads.insert(
                    entry.doc_id,
                    GraphPayload {
                        subgraph_vertices: vertices,
                        subgraph_edges: edges,
                        graph_name: String::new(),
                        score_override: Some(entry.payload.score),
                    },
                );
            }
            entries.push(PostingEntry::new(entry.doc_id, payload));
        }
        Self {
            inner: PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        }
    }

    /// Attach graph metadata to a document already present in the support.
    pub fn try_set_graph_payload(
        &mut self,
        doc_id: DocId,
        payload: GraphPayload,
    ) -> GraphPostingListResult<()> {
        if self.inner.get_entry(doc_id).is_none() {
            return Err(GraphPostingListError::PayloadOutsideSupport { doc_id });
        }
        self.graph_payloads.insert(doc_id, payload);
        Ok(())
    }

    pub fn get_graph_payload(&self, doc_id: DocId) -> Option<&GraphPayload> {
        self.graph_payloads.get(&doc_id)
    }

    pub fn inner(&self) -> &PostingList {
        &self.inner
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Union document support and ordinary payloads, while taking the set
    /// union of overlapping subgraph vertices and edges.
    pub fn merge_union(&self, other: &Self) -> GraphPostingListResult<Self> {
        self.merge_union_with(other, SubgraphMergePolicy::Union)
    }

    /// Intersect document support and ordinary payloads, while taking the set
    /// intersection of overlapping subgraph vertices and edges.
    pub fn merge_intersection(&self, other: &Self) -> GraphPostingListResult<Self> {
        self.merge_intersection_with(other, SubgraphMergePolicy::Intersection)
    }

    /// Union with an explicit graph-metadata collision policy.
    pub fn merge_union_with(
        &self,
        other: &Self,
        policy: SubgraphMergePolicy,
    ) -> GraphPostingListResult<Self> {
        let inner = self.inner.merge_union(&other.inner);
        let graph_payloads = self.merge_graph_payloads(other, &inner, policy)?;
        Self::try_from_parts(inner, graph_payloads)
    }

    /// Intersection with an explicit graph-metadata collision policy.
    pub fn merge_intersection_with(
        &self,
        other: &Self,
        policy: SubgraphMergePolicy,
    ) -> GraphPostingListResult<Self> {
        let inner = self.inner.merge_intersection(&other.inner);
        let graph_payloads = self.merge_graph_payloads(other, &inner, policy)?;
        Self::try_from_parts(inner, graph_payloads)
    }

    pub fn exclude(&self, other: &Self) -> Self {
        let inner = self.inner.exclude(&other.inner);
        let graph_payloads = self
            .graph_payloads
            .iter()
            .filter(|(doc_id, _)| inner.get_entry(**doc_id).is_some())
            .map(|(doc_id, payload)| (*doc_id, payload.clone()))
            .collect();
        Self {
            inner,
            graph_payloads,
        }
    }

    fn merge_graph_payloads(
        &self,
        other: &Self,
        merged_inner: &PostingList,
        policy: SubgraphMergePolicy,
    ) -> GraphPostingListResult<BTreeMap<DocId, GraphPayload>> {
        let mut merged = BTreeMap::new();
        for entry in merged_inner {
            let doc_id = entry.doc_id;
            let left = self.graph_payloads.get(&doc_id);
            let right = other.graph_payloads.get(&doc_id);
            let payload = match (left, right) {
                (None, None) => continue,
                (Some(payload), None) | (None, Some(payload)) => payload.clone(),
                (Some(left), Some(right)) => merge_graph_payload(doc_id, left, right, policy)?,
            };

            let overlaps =
                self.inner.get_entry(doc_id).is_some() && other.inner.get_entry(doc_id).is_some();
            let mut payload = payload;
            if overlaps
                && (left.and_then(|value| value.score_override).is_some()
                    || right.and_then(|value| value.score_override).is_some())
            {
                let left_score = effective_score(self, doc_id);
                let right_score = effective_score(other, doc_id);
                payload.score_override = Some(left_score + right_score);
            }
            merged.insert(doc_id, payload);
        }
        Ok(merged)
    }
}

fn effective_score(list: &GraphPostingList, doc_id: DocId) -> f64 {
    list.graph_payloads
        .get(&doc_id)
        .and_then(|payload| payload.score_override)
        .unwrap_or_else(|| {
            list.inner
                .get_entry(doc_id)
                .map_or(0.0, |entry| entry.payload.score)
        })
}

fn merge_graph_payload(
    doc_id: DocId,
    left: &GraphPayload,
    right: &GraphPayload,
    policy: SubgraphMergePolicy,
) -> GraphPostingListResult<GraphPayload> {
    let graph_name = match policy {
        SubgraphMergePolicy::PreferLeft => left.graph_name.clone(),
        SubgraphMergePolicy::PreferRight => right.graph_name.clone(),
        SubgraphMergePolicy::Union | SubgraphMergePolicy::Intersection => {
            compatible_graph_name(doc_id, &left.graph_name, &right.graph_name)?
        }
    };
    let (subgraph_vertices, subgraph_edges) = match policy {
        SubgraphMergePolicy::Union => (
            set_union(&left.subgraph_vertices, &right.subgraph_vertices),
            set_union(&left.subgraph_edges, &right.subgraph_edges),
        ),
        SubgraphMergePolicy::Intersection => (
            set_intersection(&left.subgraph_vertices, &right.subgraph_vertices),
            set_intersection(&left.subgraph_edges, &right.subgraph_edges),
        ),
        SubgraphMergePolicy::PreferLeft => {
            (left.subgraph_vertices.clone(), left.subgraph_edges.clone())
        }
        SubgraphMergePolicy::PreferRight => (
            right.subgraph_vertices.clone(),
            right.subgraph_edges.clone(),
        ),
    };
    Ok(GraphPayload {
        subgraph_vertices,
        subgraph_edges,
        graph_name,
        score_override: None,
    })
}

fn compatible_graph_name(doc_id: DocId, left: &str, right: &str) -> GraphPostingListResult<String> {
    if left == right || right.is_empty() {
        Ok(left.to_string())
    } else if left.is_empty() {
        Ok(right.to_string())
    } else {
        Err(GraphPostingListError::ConflictingGraphNames {
            doc_id,
            left: left.to_string(),
            right: right.to_string(),
        })
    }
}

fn set_union<T: Copy + Ord>(left: &[T], right: &[T]) -> Vec<T> {
    left.iter()
        .chain(right)
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn set_intersection<T: Copy + Ord>(left: &[T], right: &[T]) -> Vec<T> {
    let right: BTreeSet<_> = right.iter().copied().collect();
    left.iter()
        .copied()
        .filter(|value| right.contains(value))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn restore_payload_field(fields: &mut BTreeMap<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        fields.insert(key.to_string(), value);
    }
}

fn decode_id_list(value: Option<&Value>) -> Vec<u64> {
    let Some(Value::List(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| match v {
            Value::Int(n) => u64::try_from(*n).ok(),
            Value::Bytes(bytes) if bytes.len() == size_of::<u64>() => {
                let mut encoded = [0_u8; size_of::<u64>()];
                encoded.copy_from_slice(bytes);
                Some(u64::from_be_bytes(encoded))
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
