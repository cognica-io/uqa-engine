//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph-operator lowering and default graph resolution.

use super::{
    const_optional_string, const_string, const_temporal_bound, const_usize, DeepGraphDirection,
    DriverResult, Engine, OperatorTree, SQLError, SQLParam, ScalarExpr,
};

pub(super) fn lower_graph_function(
    name: &str,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Option<OperatorTree> {
    match name {
        "graph_traverse" | "traverse_match" => {
            if args.len() != 4 {
                return None;
            }
            let graph = const_string(args.first()?, params)?;
            let start_vertex = u64::try_from(const_usize(args.get(1)?, params)?).ok()?;
            let label = const_optional_string(args.get(2)?, params)?.into_option();
            let max_hops = const_usize(args.get(3)?, params)?;
            Some(OperatorTree::Traverse {
                start_vertex,
                graph,
                label,
                max_hops,
                vertex_predicate: None,
            })
        }
        "graph_neighbors" => {
            if args.len() != 4 {
                return None;
            }
            let graph = const_string(args.first()?, params)?;
            let vertex = u64::try_from(const_usize(args.get(1)?, params)?).ok()?;
            let label = const_optional_string(args.get(2)?, params)?.into_option();
            let direction = match const_string(args.get(3)?, params)?
                .to_ascii_lowercase()
                .as_str()
            {
                "out" => DeepGraphDirection::Out,
                "in" => DeepGraphDirection::In,
                "both" => DeepGraphDirection::Both,
                _ => return None,
            };
            Some(OperatorTree::GraphNeighbors {
                vertex,
                graph,
                label,
                direction,
            })
        }
        "graph_edges" => {
            if !(1..=2).contains(&args.len()) {
                return None;
            }
            Some(OperatorTree::GraphEdges {
                graph: const_string(args.first()?, params)?,
                label: match args.get(1) {
                    Some(label) => const_optional_string(label, params)?.into_option(),
                    None => None,
                },
            })
        }
        "temporal_traverse" => {
            if args.len() != 6 {
                return None;
            }
            Some(OperatorTree::TemporalTraverse {
                graph: const_string(args.first()?, params)?,
                start_vertex: u64::try_from(const_usize(args.get(1)?, params)?).ok()?,
                label: const_optional_string(args.get(2)?, params)?.into_option(),
                max_hops: const_usize(args.get(3)?, params)?,
                temporal_filter: Some(uqa_operators::TemporalFilterIR {
                    timestamp: None,
                    time_range: Some((
                        const_temporal_bound(args.get(4)?, params, f64::NEG_INFINITY)?,
                        const_temporal_bound(args.get(5)?, params, f64::INFINITY)?,
                    )),
                }),
            })
        }
        "rpq" if args.len() == 3 => Some(OperatorTree::RegularPathQuery {
            rpq_source: const_string(args.first()?, params)?,
            start_vertex: u64::try_from(const_usize(args.get(1)?, params)?).ok()?,
            graph: const_string(args.get(2)?, params)?,
        }),
        "deep_predict" if args.len() == 1 => Some(OperatorTree::DeepPredict {
            model: const_string(args.first()?, params)?,
        }),
        "graph_pagerank" | "pagerank" if args.len() == 1 => Some(OperatorTree::PageRank {
            graph: const_string(args.first()?, params)?,
        }),
        "graph_hits" | "hits" if args.len() == 1 => Some(OperatorTree::HITS {
            graph: const_string(args.first()?, params)?,
        }),
        "graph_betweenness" | "betweenness" if args.len() == 1 => {
            Some(OperatorTree::BetweennessCentrality {
                graph: const_string(args.first()?, params)?,
            })
        }
        _ => None,
    }
}

pub(super) fn default_operator_graph(engine: &Engine, function_name: &str) -> DriverResult<String> {
    let graphs = engine
        .list_graphs()
        .map_err(|error| SQLError::Internal(format!("read graph catalog: {error}")))?;
    match graphs.as_slice() {
        [graph] => Ok(graph.clone()),
        [] => Err(SQLError::Unsupported(format!(
            "{function_name} requires a graph argument because no graph is registered"
        ))),
        _ => Err(SQLError::Unsupported(format!(
            "{function_name} requires a graph argument because multiple graphs are registered: {}",
            graphs.join(", ")
        ))),
    }
}
