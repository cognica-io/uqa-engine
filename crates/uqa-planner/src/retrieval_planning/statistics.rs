//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retrieval field statistics and graph sampling from retained catalog snapshots.

use super::{
    operator_execution_error, GraphStatisticsSnapshot, PlanningResult, RetrievalPlanningCatalog,
};
use crate::{EdgeSample, GraphStats, GraphStoreSampler};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uqa_core::{IndexStats, Value, Vertex};
use uqa_operators::{tree::collect_graph_names, OperatorTree};
use uqa_sql::SQLError;

pub(super) fn index_stats(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    tree: &OperatorTree,
) -> PlanningResult<IndexStats> {
    let row_count = if table.is_empty() {
        0
    } else {
        catalog.table_doc_count(table)?
    };
    let mut stats = IndexStats::new(row_count);
    if table.is_empty() {
        return Ok(stats);
    }
    let Some(table_state) = catalog
        .try_query_table(table)
        .map_err(|error| operator_execution_error("resolve optimizer table", error))?
    else {
        return Ok(stats);
    };

    let mut text_queries = Vec::<(Option<String>, String)>::new();
    let mut vector_fields = BTreeSet::new();
    let mut query_vector_dimensions = Vec::new();
    tree.visit(&mut |node| match node {
        OperatorTree::Term { query, field, .. } => {
            text_queries.push((field.clone(), query.clone()));
        }
        OperatorTree::BayesianMatchWithPrior { field, query, .. } => {
            text_queries.push((Some(field.clone()), query.clone()));
        }
        OperatorTree::MultiFieldSearch {
            fields, queries, ..
        } => {
            text_queries.extend(
                fields
                    .iter()
                    .cloned()
                    .map(Some)
                    .zip(queries.iter().cloned()),
            );
        }
        OperatorTree::VectorSimilarity {
            field,
            query_vector,
            ..
        }
        | OperatorTree::KNN {
            field,
            query_vector,
            ..
        }
        | OperatorTree::CalibratedVectorMatch {
            field,
            query_vector,
            ..
        } => {
            vector_fields.insert(field.clone());
            query_vector_dimensions.push(query_vector.len());
        }
        _ => {}
    });

    {
        let index = table_state.text_index();
        for (field, query) in text_queries {
            let (stats_field, document_frequency) = if let Some(field) = field {
                let terms = index
                    .analyze(&field, &query)
                    .map_err(|error| operator_execution_error("analyze optimizer query", error))?;
                let mut document_frequency = 0_u64;
                for term in terms {
                    document_frequency =
                        document_frequency.saturating_add(index.doc_freq(&field, &term).map_err(
                            |error| operator_execution_error("read document frequency", error),
                        )?);
                }
                (field, document_frequency.min(row_count))
            } else {
                let document_frequency = index
                    .doc_freq_any_field(&query)
                    .map_err(|error| operator_execution_error("read document frequency", error))?;
                ("_default".to_string(), document_frequency.min(row_count))
            };
            stats.set_doc_freq(stats_field, query, document_frequency);
        }
    }

    let vector_indexes = table_state.vector_indexes();
    let indexed_dimensions = vector_fields
        .iter()
        .filter_map(|field| vector_indexes.dimensions(field))
        .max()
        .unwrap_or(0);
    let query_dimensions = query_vector_dimensions
        .into_iter()
        .filter_map(|dimensions| u32::try_from(dimensions).ok())
        .max()
        .unwrap_or(0);
    stats.dimensions = indexed_dimensions.max(query_dimensions);
    Ok(stats)
}

#[derive(Clone)]
struct GraphSamplerSnapshot {
    vertices: BTreeMap<u64, Vertex>,
    outgoing: BTreeMap<u64, Vec<(u64, String)>>,
}

impl GraphStoreSampler for GraphSamplerSnapshot {
    fn vertex_ids(&self) -> Vec<u64> {
        self.vertices.keys().copied().collect()
    }

    fn outgoing_edges(&self, vid: u64) -> Vec<EdgeSample> {
        self.outgoing
            .get(&vid)
            .into_iter()
            .flatten()
            .map(|(target_id, label)| EdgeSample {
                target_id: *target_id,
                label: label.clone(),
            })
            .collect()
    }

    fn vertex_satisfies(&self, vid: u64, constraint: &uqa_operators::VertexConstraint) -> bool {
        self.vertices
            .get(&vid)
            .is_some_and(|vertex| constraint(vertex))
    }
}

pub(super) fn graph_context(
    catalog: &dyn RetrievalPlanningCatalog,
    tree: &OperatorTree,
) -> PlanningResult<Option<(GraphStats, Arc<dyn GraphStoreSampler>)>> {
    let mut graph_names = BTreeSet::new();
    collect_graph_names(tree, &mut graph_names);
    let Some(graph_name) = graph_names.iter().next() else {
        return Ok(None);
    };
    if graph_names.len() != 1 {
        return Ok(None);
    }
    let graph_name = graph_name.clone();
    let snapshot = catalog
        .graph_snapshot(&graph_name)
        .map_err(|error| operator_execution_error("read graph statistics", error))?
        .ok_or_else(|| SQLError::Unsupported(format!("graph `{graph_name}` does not exist")))?;
    let GraphStatisticsSnapshot {
        vertices,
        edges,
        degree_distribution,
        vertex_label_counts,
    } = snapshot;

    let mut label_counts = BTreeMap::<String, u64>::new();
    let mut outgoing = BTreeMap::<u64, Vec<(u64, String)>>::new();
    let mut min_timestamp: Option<f64> = None;
    let mut max_timestamp: Option<f64> = None;
    for edge in &edges {
        *label_counts.entry(edge.label.clone()).or_default() += 1;
        outgoing
            .entry(edge.source_id)
            .or_default()
            .push((edge.target_id, edge.label.clone()));
        for key in ["valid_from", "valid_to"] {
            let timestamp = match edge.properties.get(key) {
                Some(Value::Float(value)) => Some(*value),
                Some(Value::Int(value)) => Some(*value as f64),
                _ => None,
            };
            if let Some(timestamp) = timestamp.filter(|value| value.is_finite()) {
                min_timestamp = Some(min_timestamp.map_or(timestamp, |old| old.min(timestamp)));
                max_timestamp = Some(max_timestamp.map_or(timestamp, |old| old.max(timestamp)));
            }
        }
    }
    for values in outgoing.values_mut() {
        values.sort();
    }

    let num_vertices = u64::try_from(vertices.len())
        .map_err(|_| SQLError::Internal("graph vertex count exceeds u64".into()))?;
    let num_edges = u64::try_from(edges.len())
        .map_err(|_| SQLError::Internal("graph edge count exceeds u64".into()))?;
    let avg_out_degree = if num_vertices == 0 {
        0.0
    } else {
        num_edges as f64 / num_vertices as f64
    };
    let label_degree_map = label_counts
        .iter()
        .map(|(label, count)| {
            let degree = if num_vertices == 0 {
                0.0
            } else {
                *count as f64 / num_vertices as f64
            };
            (label.clone(), degree)
        })
        .collect();
    let graph_stats = GraphStats {
        num_vertices,
        num_edges,
        label_counts,
        avg_out_degree,
        degree_distribution,
        min_timestamp,
        max_timestamp,
        graph_name,
        vertex_label_counts,
        label_degree_map,
    };
    let sampler = GraphSamplerSnapshot {
        vertices: vertices
            .into_iter()
            .map(|vertex| (vertex.vertex_id, vertex))
            .collect(),
        outgoing,
    };
    Ok(Some((graph_stats, Arc::new(sampler))))
}
