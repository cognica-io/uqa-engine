//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable graph snapshots and IR-to-runtime graph conversion.

use super::{
    graph_execution_error, BTreeMap, BTreeSet, DocId, DriverResult, Payload, PostingEntry,
    PostingList, SQLError, StorageBackendError,
};

#[derive(Default)]
pub(super) struct GraphNeighborSnapshot {
    vertices: BTreeSet<u64>,
    out: BTreeMap<u64, Vec<(String, u64)>>,
    incoming: BTreeMap<u64, Vec<(String, u64)>>,
}

impl GraphNeighborSnapshot {
    pub(super) fn from_store(
        store: &uqa_graph::MemoryGraphStore,
        graph: &str,
    ) -> DriverResult<Self> {
        use uqa_graph::GraphStore;

        let vertices = store
            .vertex_ids_in_graph(graph)
            .map_err(|error| graph_execution_error("DeepFusion graph snapshot", error))?;
        let mut snapshot = Self {
            vertices,
            ..Self::default()
        };
        for edge in store
            .edges_in_graph(graph)
            .map_err(|error| graph_execution_error("DeepFusion graph snapshot", error))?
        {
            snapshot
                .out
                .entry(edge.source_id)
                .or_default()
                .push((edge.label.clone(), edge.target_id));
            snapshot
                .incoming
                .entry(edge.target_id)
                .or_default()
                .push((edge.label, edge.source_id));
        }
        Ok(snapshot)
    }
}

impl uqa_operators::GraphNeighborLookup for GraphNeighborSnapshot {
    fn neighbors(
        &self,
        vertex: u64,
        label: &str,
        direction: uqa_operators::DeepGraphDirection,
    ) -> uqa_storage::StorageBackendResult<Vec<u64>> {
        if !self.vertices.contains(&vertex) {
            return Err(StorageBackendError::Other(format!(
                "graph-aware DeepFusion input vertex {vertex} is not a member of the selected graph"
            )));
        }
        let mut result = Vec::new();
        let mut append = |edges: Option<&Vec<(String, u64)>>| {
            if let Some(edges) = edges {
                result.extend(
                    edges
                        .iter()
                        .filter(|(edge_label, _)| label.is_empty() || edge_label == label)
                        .map(|(_, neighbor)| *neighbor),
                );
            }
        };
        if matches!(
            direction,
            uqa_operators::DeepGraphDirection::Out | uqa_operators::DeepGraphDirection::Both
        ) {
            append(self.out.get(&vertex));
        }
        if matches!(
            direction,
            uqa_operators::DeepGraphDirection::In | uqa_operators::DeepGraphDirection::Both
        ) {
            append(self.incoming.get(&vertex));
        }
        result.sort_unstable();
        result.dedup();
        Ok(result)
    }
}

pub(super) fn graph_pattern_from_ir(
    pattern: &uqa_operators::GraphPatternIR,
) -> uqa_graph::GraphPattern {
    let mut converted = uqa_graph::GraphPattern::new();
    for vertex in &pattern.vertex_patterns {
        let mut converted_vertex = uqa_graph::VertexPattern::new(&vertex.variable);
        if let Some(label) = &vertex.label {
            converted_vertex =
                converted_vertex.with(uqa_graph::VertexPredicate::LabelEq(label.clone()));
        }
        for constraint in &vertex.constraints {
            converted_vertex =
                converted_vertex.with(uqa_graph::VertexPredicate::Custom(constraint.clone()));
        }
        converted = converted.add_vertex(converted_vertex);
    }
    for edge in &pattern.edge_patterns {
        let mut converted_edge = uqa_graph::EdgePattern::new(&edge.source_var, &edge.target_var);
        if let Some(label) = &edge.label {
            converted_edge = converted_edge.with_label(label);
        }
        for constraint in &edge.constraints {
            converted_edge =
                converted_edge.with(uqa_graph::EdgePredicate::Custom(constraint.clone()));
        }
        converted = converted.add_edge(converted_edge);
    }
    converted
}

pub(super) fn parse_rpq(source: &str) -> DriverResult<uqa_graph::RegularPathExpr> {
    uqa_graph::parse_rpq(source)
        .map_err(|error| SQLError::TypeMismatch(format!("invalid RPQ {source:?}: {error}")))
}

pub(super) fn temporal_filter_from_ir(
    filter: Option<&uqa_operators::TemporalFilterIR>,
) -> DriverResult<uqa_graph::TemporalFilter> {
    let Some(filter) = filter else {
        return Ok(uqa_graph::TemporalFilter::Any);
    };
    if filter.timestamp.is_some_and(f64::is_nan) {
        return Err(SQLError::TypeMismatch(
            "temporal timestamp cannot be NaN".to_string(),
        ));
    }
    if let Some((start, end)) = filter.time_range {
        if start.is_nan() || end.is_nan() || start > end {
            return Err(SQLError::TypeMismatch(format!(
                "temporal range must be ordered and non-NaN, got [{start}, {end}]"
            )));
        }
    }
    match (filter.timestamp, filter.time_range) {
        (Some(timestamp), Some((start, end))) => Ok(uqa_graph::TemporalFilter::TimestampAndRange(
            timestamp, start, end,
        )),
        (Some(timestamp), None) => Ok(uqa_graph::TemporalFilter::Timestamp(timestamp)),
        (None, Some((start, end))) => Ok(uqa_graph::TemporalFilter::Range(start, end)),
        (None, None) => Ok(uqa_graph::TemporalFilter::Any),
    }
}

pub(super) fn restrict_result_to_source(result: &PostingList, source: &PostingList) -> PostingList {
    let source_by_id: BTreeMap<DocId, &Payload> = source
        .entries()
        .iter()
        .map(|entry| (entry.doc_id, &entry.payload))
        .collect();
    let entries = result
        .entries()
        .iter()
        .filter_map(|entry| {
            let source_payload = source_by_id.get(&entry.doc_id)?;
            let mut payload = entry.payload.clone();
            for (field, value) in &source_payload.fields {
                payload
                    .fields
                    .entry(field.clone())
                    .or_insert_with(|| value.clone());
            }
            Some(PostingEntry::new(entry.doc_id, payload))
        })
        .collect();
    PostingList::from_sorted_unchecked(entries)
}
