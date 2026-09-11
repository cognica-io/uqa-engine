//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime-independent operator-tree graph references.

use super::OperatorTree;
use std::collections::BTreeSet;

pub fn collect_graph_names(tree: &OperatorTree, names: &mut BTreeSet<String>) {
    tree.visit(&mut |node| {
        let graph = match node {
            OperatorTree::Traverse { graph, .. }
            | OperatorTree::PatternMatch { graph, .. }
            | OperatorTree::RegularPathQuery { graph, .. }
            | OperatorTree::WeightedPathQuery { graph, .. }
            | OperatorTree::GraphJoin { graph, .. }
            | OperatorTree::PageRank { graph }
            | OperatorTree::HITS { graph }
            | OperatorTree::BetweennessCentrality { graph }
            | OperatorTree::TemporalTraverse { graph, .. }
            | OperatorTree::TemporalPatternMatch { graph, .. } => Some(graph),
            _ => None,
        };
        if let Some(graph) = graph {
            names.insert(graph.clone());
        }
    });
}
