//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine catalog binding for optimizer costs and index candidates.

use super::{
    collect_graph_names, operator_execution_error, BTreeMap, BTreeSet, DriverResult, Engine,
    IndexScanCandidate, OperatorTree, PathSegment, QueryOptimizer, SQLError, Value,
};
use std::sync::Arc;

use uqa_core::{IndexStats, Vertex};
use uqa_planner::{
    AccessParadigm, CostEstimator, EdgeSample, GraphStats, GraphStoreSampler, OperatorKind,
};

pub(super) fn engine_query_optimizer(
    engine: &Engine,
    table: &str,
    tree: &OperatorTree,
) -> DriverResult<QueryOptimizer> {
    let candidates = engine_index_candidates(engine, table, tree)?;
    let stats = engine_index_stats(engine, table, tree)?;
    let column_stats = if table.is_empty() {
        BTreeMap::new()
    } else {
        engine.try_column_stats(table).map_err(|error| {
            SQLError::Internal(format!(
                "read optimizer column statistics for `{table}`: {error}"
            ))
        })?
    };
    let mut optimizer = QueryOptimizer::new()
        .with_index_stats(stats)
        .with_column_stats(column_stats)
        .with_index_candidates(candidates, table);
    if let Some((graph_stats, graph_store)) = engine_graph_context(engine, tree)? {
        optimizer = optimizer
            .with_graph_stats(graph_stats)
            .with_graph_store(graph_store);
    }
    Ok(optimizer)
}

fn engine_index_stats(
    engine: &Engine,
    table: &str,
    tree: &OperatorTree,
) -> DriverResult<IndexStats> {
    let row_count = if table.is_empty() {
        0
    } else {
        engine.table_doc_count(table)?
    };
    let mut stats = IndexStats::new(row_count);
    if table.is_empty() {
        return Ok(stats);
    }
    let Some(table_state) = engine
        .try_table(table)
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
        let index = table_state.inverted_index.read();
        for (field, query) in text_queries {
            let (stats_field, document_frequency) = if let Some(field) = field {
                let terms = index
                    .get_search_analyzer(&field)
                    .analyze(&query)
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

    let vector_indexes = table_state.vector_indexes.read();
    let indexed_dimensions = vector_fields
        .iter()
        .filter_map(|field| vector_indexes.get(field).map(|index| index.dimensions()))
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

fn engine_graph_context(
    engine: &Engine,
    tree: &OperatorTree,
) -> DriverResult<Option<(GraphStats, Arc<dyn GraphStoreSampler>)>> {
    let mut graph_names = BTreeSet::new();
    collect_graph_names(tree, &mut graph_names);
    let Some(graph_name) = graph_names.iter().next() else {
        return Ok(None);
    };
    if graph_names.len() != 1 {
        return Ok(None);
    }
    let graph_name = graph_name.clone();
    let snapshot = engine
        .graph_with(&graph_name, |store| {
            use uqa_graph::GraphStore as _;
            let vertices = store.vertices_in_graph(&graph_name)?;
            let edges = store.edges_in_graph(&graph_name)?;
            let degree_distribution = store.degree_distribution(&graph_name)?;
            let vertex_label_counts = store.vertex_label_counts(&graph_name)?;
            Ok::<_, uqa_graph::GraphStoreError>((
                vertices,
                edges,
                degree_distribution,
                vertex_label_counts,
            ))
        })
        .map_err(|error| operator_execution_error("read graph statistics", error))?
        .ok_or_else(|| SQLError::Unsupported(format!("graph `{graph_name}` does not exist")))?
        .map_err(|error| operator_execution_error("read graph statistics", error))?;
    let (vertices, edges, degree_distribution, vertex_label_counts) = snapshot;

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

pub(super) fn operator_tree_paradigm(tree: &OperatorTree) -> AccessParadigm {
    let mut text = false;
    let mut vector = false;
    let mut graph = false;
    let mut relational = false;
    let mut text_join = false;
    let mut vector_join = false;
    let mut graph_join = false;
    let mut hybrid_join = false;
    let mut cross_paradigm_join = false;
    tree.visit(&mut |node| match node {
        OperatorTree::Term { .. }
        | OperatorTree::BayesianScore { .. }
        | OperatorTree::BayesianMatchWithPrior { .. }
        | OperatorTree::MultiFieldSearch { .. } => text = true,
        OperatorTree::VectorSimilarity { .. }
        | OperatorTree::KNN { .. }
        | OperatorTree::CalibratedVectorMatch { .. }
        | OperatorTree::CosineProbability(_) => vector = true,
        OperatorTree::Traverse { .. }
        | OperatorTree::GraphNeighbors { .. }
        | OperatorTree::GraphEdges { .. }
        | OperatorTree::PatternMatch { .. }
        | OperatorTree::RegularPathQuery { .. }
        | OperatorTree::WeightedPathQuery { .. }
        | OperatorTree::PageRank { .. }
        | OperatorTree::HITS { .. }
        | OperatorTree::BetweennessCentrality { .. }
        | OperatorTree::TemporalTraverse { .. }
        | OperatorTree::TemporalPatternMatch { .. } => graph = true,
        OperatorTree::Filter { .. } | OperatorTree::IndexScan { .. } => relational = true,
        OperatorTree::TextSimilarityJoin { .. } => text_join = true,
        OperatorTree::VectorSimilarityJoin { .. } => vector_join = true,
        OperatorTree::GraphJoin { .. } => graph_join = true,
        OperatorTree::HybridJoin { .. } => hybrid_join = true,
        OperatorTree::CrossParadigmJoin { .. } => cross_paradigm_join = true,
        _ => {}
    });
    if cross_paradigm_join || graph && (text || vector || relational) {
        AccessParadigm::CrossParadigm
    } else if hybrid_join || text && vector || relational && (text || vector) {
        AccessParadigm::Hybrid
    } else if graph_join || graph {
        AccessParadigm::Graph
    } else if vector_join || vector {
        AccessParadigm::Vector
    } else if text_join || text {
        AccessParadigm::Text
    } else {
        AccessParadigm::Relational
    }
}

pub(super) fn engine_index_candidates(
    engine: &Engine,
    table: &str,
    tree: &OperatorTree,
) -> DriverResult<Vec<IndexScanCandidate>> {
    if table.is_empty()
        || !engine
            .has_table(table)
            .map_err(|error| operator_execution_error("resolve index candidate table", error))?
    {
        return Ok(Vec::new());
    }
    let resolved_table = engine
        .resolve_table_name(table)
        .map_err(|error| operator_execution_error("resolve index candidate table", error))?
        .unwrap_or_else(|| table.to_string());
    let mut indexes_by_field = BTreeMap::new();
    for index in engine
        .list_catalog_indexes()
        .map_err(|error| operator_execution_error("list index candidates", error))?
    {
        if index.table_name != resolved_table || !index.index_type.eq_ignore_ascii_case("btree") {
            continue;
        }
        let columns =
            serde_json::from_str::<Vec<String>>(&index.columns_json).map_err(|error| {
                SQLError::Internal(format!(
                    "decode catalog index `{}` columns: {error}",
                    index.name
                ))
            })?;
        if let Some(field) = columns.first() {
            indexes_by_field
                .entry(field.clone())
                .or_insert(index.name.clone());
        }
    }

    let mut predicates = Vec::new();
    tree.visit(&mut |node| {
        let OperatorTree::Filter {
            field,
            predicate,
            source: None,
        } = node
        else {
            return;
        };
        predicates.push((field.clone(), predicate.clone()));
    });

    let mut candidates = Vec::new();
    for (field, predicate) in predicates {
        let Some(index_name) = indexes_by_field.get(&field) else {
            continue;
        };
        let Some(cardinality) = engine.value_index_cardinality(table, &field, &predicate)? else {
            continue;
        };
        let cardinality = cardinality as f64;
        let scan_cost = CostEstimator::default()
            .estimate_unary(OperatorKind::IndexScan, cardinality)
            .total();
        candidates.push(IndexScanCandidate {
            index_name: index_name.clone(),
            table_name: table.to_string(),
            field,
            predicate,
            scan_cost,
        });
    }
    Ok(candidates)
}

/// Number of score-contributing text terms in a bound BM25 query tree.
/// Set operations merge payloads by summing scores, so the raw query
/// score scales with this count and the calibration must be translated
/// to it. Complements filter without contributing score.
pub(super) fn scored_term_count(tree: &OperatorTree) -> usize {
    match tree {
        OperatorTree::Term { .. } => 1,
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().map(scored_term_count).sum(),
        OperatorTree::Filter { source, .. } => source.as_deref().map_or(0, scored_term_count),
        OperatorTree::BayesianScore { source, .. } | OperatorTree::Score { source, .. } => {
            scored_term_count(source)
        }
        _ => 0,
    }
}

// `eval_path` lives in storage; expose a shim so we don't pull in the
// trait at the lowering layer just for this helper.
#[allow(dead_code)]
pub(super) fn lookup_path(value: &Value, path: &[PathSegment]) -> Option<Value> {
    let mut current = value.clone();
    for seg in path {
        current = match (current, seg) {
            (Value::Map(m), PathSegment::Key(k)) => m.get(k)?.clone(),
            (Value::List(items), PathSegment::Index(i)) => items.get(*i)?.clone(),
            _ => return None,
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{Edge, Predicate, Vertex};

    #[test]
    fn query_optimizer_binds_live_graph_statistics_and_sampler() {
        let engine = Engine::new();
        engine.create_graph("citations").unwrap();
        for (id, year) in [(1, 2024), (2, 2023), (3, 2024)] {
            let mut vertex = Vertex::new(id, "Paper");
            vertex.properties.insert("year".into(), Value::Int(year));
            engine.add_graph_vertex(vertex, "citations").unwrap();
        }
        let mut first = Edge::new(1, 1, 2, "cites");
        first
            .properties
            .insert("valid_from".into(), Value::Float(10.0));
        let mut second = Edge::new(2, 1, 3, "cites");
        second
            .properties
            .insert("valid_to".into(), Value::Float(20.0));
        engine.add_graph_edge(first, "citations").unwrap();
        engine.add_graph_edge(second, "citations").unwrap();

        let tree = OperatorTree::Traverse {
            start_vertex: 1,
            graph: "citations".into(),
            label: Some("cites".into()),
            max_hops: 2,
            vertex_predicate: None,
        };
        let optimizer = engine_query_optimizer(&engine, "", &tree).unwrap();
        let stats = optimizer
            .graph_stats
            .as_ref()
            .expect("live graph statistics must be bound");
        assert_eq!(stats.graph_name, "citations");
        assert_eq!(stats.num_vertices, 3);
        assert_eq!(stats.num_edges, 2);
        assert_eq!(stats.label_counts.get("cites"), Some(&2));
        assert_eq!(stats.vertex_label_counts.get("Paper"), Some(&3));
        assert_eq!(stats.min_timestamp, Some(10.0));
        assert_eq!(stats.max_timestamp, Some(20.0));

        let sampler = optimizer
            .estimator
            .graph_store
            .as_ref()
            .expect("live graph sampler must be bound");
        assert_eq!(sampler.vertex_ids(), vec![1, 2, 3]);
        assert_eq!(sampler.outgoing_edges(1).len(), 2);
        let year_2024: uqa_operators::VertexConstraint =
            Arc::new(|vertex| vertex.properties.get("year") == Some(&Value::Int(2024)));
        assert!(sampler.vertex_satisfies(1, &year_2024));
        assert!(!sampler.vertex_satisfies(2, &year_2024));
    }

    #[test]
    fn query_optimizer_binds_analyzed_column_statistics() {
        let engine = Engine::new();
        engine
            .sql(
                "CREATE TABLE stats_docs (id INTEGER PRIMARY KEY, category INTEGER)",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO stats_docs (id, category) VALUES \
                 (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), \
                 (6, 6), (7, 7), (8, 8), (9, 9), (10, 10)",
                &[],
            )
            .unwrap();
        engine.sql("ANALYZE stats_docs", &[]).unwrap();
        let tree = OperatorTree::Filter {
            field: "category".into(),
            predicate: Predicate::Equals(Value::Int(4)),
            source: None,
        };

        let optimizer = engine_query_optimizer(&engine, "stats_docs", &tree).unwrap();
        let category = optimizer
            .estimator
            .column_stats
            .get("category")
            .expect("ANALYZE statistics must reach the operator optimizer");
        assert_eq!(category.distinct_count, 10);
        assert_eq!(category.row_count, 10);
        let cost_category = optimizer
            .cost_model
            .column_stats
            .get("category")
            .expect("ANALYZE statistics must reach the operator cost model");
        assert_eq!(cost_category.distinct_count, 10);
        assert_eq!(cost_category.row_count, 10);
        let estimated = optimizer.estimator.estimate(&tree, &optimizer.index_stats);
        assert!(estimated >= 1.0);
        assert!(estimated < 2.0);
    }

    #[test]
    fn operator_join_paradigms_remain_specific() {
        let term = || OperatorTree::Term {
            query: "rust".into(),
            field: Some("body".into()),
            scoring: None,
            top_k: None,
        };
        let vector = || OperatorTree::KNN {
            query_vector: vec![1.0, 0.0],
            k: 3,
            field: "embedding".into(),
        };
        let filter = || OperatorTree::Filter {
            field: "category".into(),
            predicate: Predicate::IsNotNull,
            source: None,
        };
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::TextSimilarityJoin {
                left: Box::new(term()),
                right: Box::new(term()),
                threshold: 0.5,
            }),
            AccessParadigm::Text
        );
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::VectorSimilarityJoin {
                left: Box::new(vector()),
                right: Box::new(vector()),
                threshold: 0.5,
            }),
            AccessParadigm::Vector
        );
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::GraphJoin {
                left: Box::new(OperatorTree::PageRank { graph: "g".into() }),
                right: Box::new(OperatorTree::PageRank { graph: "g".into() }),
                label: None,
                graph: "g".into(),
            }),
            AccessParadigm::Graph
        );
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::HybridJoin {
                left: Box::new(OperatorTree::Intersect(vec![filter(), vector()])),
                right: Box::new(OperatorTree::Intersect(vec![filter(), vector()])),
            }),
            AccessParadigm::Hybrid
        );
        assert_eq!(
            operator_tree_paradigm(&OperatorTree::CrossParadigmJoin {
                left: Box::new(OperatorTree::PageRank { graph: "g".into() }),
                right: Box::new(filter()),
            }),
            AccessParadigm::CrossParadigm
        );
    }
}
