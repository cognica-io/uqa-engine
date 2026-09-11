//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph retrieval arguments and logical expression construction.

use super::{
    const_optional_string, const_string, const_temporal_bound, const_usize, DeepGraphDirection,
    RetrievalConstants, RetrievalExpr, ScalarExpr,
};

pub(super) fn lower_graph_function(
    name: &str,
    args: &[ScalarExpr],
    constants: &RetrievalConstants<'_>,
) -> Option<RetrievalExpr> {
    match name {
        "graph_traverse" | "traverse_match" => {
            if args.len() != 4 {
                return None;
            }
            let graph = const_string(args.first()?, constants)?;
            let start_vertex = u64::try_from(const_usize(args.get(1)?, constants)?).ok()?;
            let label = const_optional_string(args.get(2)?, constants)?.into_option();
            let max_hops = const_usize(args.get(3)?, constants)?;
            Some(RetrievalExpr::Traverse {
                start_vertex,
                graph,
                label,
                max_hops,
            })
        }
        "graph_neighbors" => {
            if args.len() != 4 {
                return None;
            }
            let graph = const_string(args.first()?, constants)?;
            let vertex = u64::try_from(const_usize(args.get(1)?, constants)?).ok()?;
            let label = const_optional_string(args.get(2)?, constants)?.into_option();
            let direction = match const_string(args.get(3)?, constants)?
                .to_ascii_lowercase()
                .as_str()
            {
                "out" => DeepGraphDirection::Out,
                "in" => DeepGraphDirection::In,
                "both" => DeepGraphDirection::Both,
                _ => return None,
            };
            Some(RetrievalExpr::GraphNeighbors {
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
            Some(RetrievalExpr::GraphEdges {
                graph: const_string(args.first()?, constants)?,
                label: match args.get(1) {
                    Some(label) => const_optional_string(label, constants)?.into_option(),
                    None => None,
                },
            })
        }
        "temporal_traverse" => {
            if args.len() != 6 {
                return None;
            }
            Some(RetrievalExpr::TemporalTraverse {
                graph: const_string(args.first()?, constants)?,
                start_vertex: u64::try_from(const_usize(args.get(1)?, constants)?).ok()?,
                label: const_optional_string(args.get(2)?, constants)?.into_option(),
                max_hops: const_usize(args.get(3)?, constants)?,
                temporal_filter: Some(uqa_core::retrieval::TemporalFilterIR {
                    timestamp: None,
                    time_range: Some((
                        const_temporal_bound(args.get(4)?, constants, f64::NEG_INFINITY)?,
                        const_temporal_bound(args.get(5)?, constants, f64::INFINITY)?,
                    )),
                }),
            })
        }
        "rpq" if args.len() == 3 => Some(RetrievalExpr::RegularPathQuery {
            rpq_source: const_string(args.first()?, constants)?,
            start_vertex: u64::try_from(const_usize(args.get(1)?, constants)?).ok()?,
            graph: const_string(args.get(2)?, constants)?,
        }),
        "deep_predict" if args.len() == 1 => Some(RetrievalExpr::DeepPredict {
            model: const_string(args.first()?, constants)?,
        }),
        "graph_pagerank" | "pagerank" if args.len() == 1 => Some(RetrievalExpr::PageRank {
            graph: const_string(args.first()?, constants)?,
        }),
        "graph_hits" | "hits" if args.len() == 1 => Some(RetrievalExpr::HITS {
            graph: const_string(args.first()?, constants)?,
        }),
        "graph_betweenness" | "betweenness" if args.len() == 1 => {
            Some(RetrievalExpr::BetweennessCentrality {
                graph: const_string(args.first()?, constants)?,
            })
        }
        _ => None,
    }
}
