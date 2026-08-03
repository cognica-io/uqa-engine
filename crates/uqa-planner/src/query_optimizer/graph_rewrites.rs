//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph-specific predicate pushdown and pattern fusion rewrites.

use std::sync::Arc;

use uqa_operators::{
    EdgePatternIR, GraphPatternIR, OperatorTree, VertexPatternIR, VertexPredicate,
};

use super::{tree_map::map_operator_children, QueryOptimizer};

impl QueryOptimizer {
    // ---------------------------------------------------------------
    // 3. Push graph pattern filters
    // ---------------------------------------------------------------

    pub(super) fn push_graph_pattern_filters(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = &op
        {
            if let OperatorTree::PatternMatch { pattern, graph } = s.as_ref() {
                let parts: Vec<&str> = field.splitn(2, '.').collect();
                if parts.len() == 2 {
                    let target_var = parts[0];
                    let prop = parts[1].to_string();
                    let mut new_vps = Vec::with_capacity(pattern.vertex_patterns.len());
                    let mut pushed = false;
                    for vp in &pattern.vertex_patterns {
                        if !pushed && vp.variable == target_var {
                            let mut constraints = vp.constraints.clone();
                            let pred = predicate.clone();
                            let prop_clone = prop.clone();
                            constraints.push(Arc::new(move |v: &uqa_core::Vertex| {
                                v.properties
                                    .get(&prop_clone)
                                    .is_some_and(|val| pred.evaluate(Some(val)))
                            }));
                            new_vps.push(VertexPatternIR {
                                variable: vp.variable.clone(),
                                constraints,
                                label: vp.label.clone(),
                            });
                            pushed = true;
                        } else {
                            new_vps.push(vp.clone());
                        }
                    }
                    if pushed {
                        let new_pattern = GraphPatternIR {
                            vertex_patterns: new_vps,
                            edge_patterns: pattern.edge_patterns.clone(),
                        };
                        return OperatorTree::PatternMatch {
                            pattern: new_pattern,
                            graph: graph.clone(),
                        };
                    }
                    // Try edge variable: "src_tgt.prop". Use a two-stage
                    // match: first build the
                    // `source_target -> ep` lookup (last-write-wins so
                    // multi-label edges between the same vertices fall
                    // through to the most recently declared label),
                    // then push the new constraint onto *every* edge
                    // sharing the chosen triple `(source, target, label)`.
                    let mut edge_lookup: std::collections::BTreeMap<String, EdgePatternIR> =
                        std::collections::BTreeMap::new();
                    for ep in &pattern.edge_patterns {
                        let key = format!("{}_{}", ep.source_var, ep.target_var);
                        edge_lookup.insert(key, ep.clone());
                    }
                    let chosen = edge_lookup.get(target_var).cloned();
                    let mut new_eps = Vec::with_capacity(pattern.edge_patterns.len());
                    if let Some(ref chosen_ep) = chosen {
                        for orig_ep in &pattern.edge_patterns {
                            if orig_ep.source_var == chosen_ep.source_var
                                && orig_ep.target_var == chosen_ep.target_var
                                && orig_ep.label == chosen_ep.label
                            {
                                let mut constraints = orig_ep.constraints.clone();
                                let pred = predicate.clone();
                                let prop_clone = prop.clone();
                                constraints.push(Arc::new(move |e: &uqa_core::Edge| {
                                    e.properties
                                        .get(&prop_clone)
                                        .is_some_and(|val| pred.evaluate(Some(val)))
                                }));
                                new_eps.push(EdgePatternIR {
                                    source_var: orig_ep.source_var.clone(),
                                    target_var: orig_ep.target_var.clone(),
                                    label: orig_ep.label.clone(),
                                    constraints,
                                });
                                pushed = true;
                            } else {
                                new_eps.push(orig_ep.clone());
                            }
                        }
                    }
                    if pushed {
                        let new_pattern = GraphPatternIR {
                            vertex_patterns: pattern.vertex_patterns.clone(),
                            edge_patterns: new_eps,
                        };
                        return OperatorTree::PatternMatch {
                            pattern: new_pattern,
                            graph: graph.clone(),
                        };
                    }
                }
                // Cannot push; keep the filter wrapped.
                return OperatorTree::Filter {
                    field: field.clone(),
                    predicate: predicate.clone(),
                    source: Some(Box::new(s.as_ref().clone())),
                };
            }
        }
        self.recurse_graph_pattern(op)
    }

    pub(super) fn recurse_graph_pattern(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.push_graph_pattern_filters(child))
    }

    // ---------------------------------------------------------------
    // 4. Push filter into Traverse
    // ---------------------------------------------------------------

    pub(super) fn push_filter_into_traverse(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = &op
        {
            if let OperatorTree::Traverse {
                start_vertex,
                graph,
                label,
                max_hops,
                vertex_predicate,
            } = s.as_ref()
            {
                let pred = predicate.clone();
                let field_clone = field.clone();
                let leaf_filter: VertexPredicate = Arc::new(move |v: &uqa_core::Vertex| {
                    v.properties
                        .get(&field_clone)
                        .is_some_and(|val| pred.evaluate(Some(val)))
                });
                let combined: VertexPredicate = match vertex_predicate {
                    Some(prev) => {
                        let prev = prev.clone();
                        Arc::new(move |v: &uqa_core::Vertex| prev(v) && leaf_filter(v))
                    }
                    None => leaf_filter,
                };
                return OperatorTree::Traverse {
                    start_vertex: *start_vertex,
                    graph: graph.clone(),
                    label: label.clone(),
                    max_hops: *max_hops,
                    vertex_predicate: Some(combined),
                };
            }
        }
        self.recurse_traverse_filter(op)
    }

    pub(super) fn recurse_traverse_filter(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.push_filter_into_traverse(child))
    }

    // ---------------------------------------------------------------
    // 5. Push filter below GraphJoin
    // ---------------------------------------------------------------

    pub(super) fn push_filter_below_graph_join(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = op
        {
            if let OperatorTree::GraphJoin {
                left,
                right,
                label,
                graph,
            } = *s
            {
                let new_left = OperatorTree::Filter {
                    field: field.clone(),
                    predicate: predicate.clone(),
                    source: Some(left),
                };
                return OperatorTree::GraphJoin {
                    left: Box::new(new_left),
                    right,
                    label,
                    graph,
                };
            }
            return OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.push_filter_below_graph_join(*s))),
            };
        }
        self.recurse_graph_join(op)
    }

    pub(super) fn recurse_graph_join(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.push_filter_below_graph_join(child))
    }

    // ---------------------------------------------------------------
    // 6. Fuse PatternMatch operators inside an Intersect
    // ---------------------------------------------------------------

    pub(super) fn fuse_join_pattern(op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Intersect(ops) = op {
            let children: Vec<OperatorTree> =
                ops.into_iter().map(Self::fuse_join_pattern).collect();
            let mut pattern_ops: Vec<OperatorTree> = Vec::new();
            let mut other_ops: Vec<OperatorTree> = Vec::new();
            for child in children {
                if matches!(child, OperatorTree::PatternMatch { .. }) {
                    pattern_ops.push(child);
                } else {
                    other_ops.push(child);
                }
            }
            // Pairwise fuse PatternMatch operators that share a vertex
            // variable.
            if pattern_ops.len() >= 2 {
                let mut merged: Vec<OperatorTree> = Vec::with_capacity(pattern_ops.len());
                merged.push(pattern_ops.remove(0));
                for pm in pattern_ops {
                    let Some(last) = merged.pop() else {
                        merged.push(pm);
                        continue;
                    };
                    match Self::merge_patterns(&last, &pm) {
                        Some(fused) => merged.push(fused),
                        None => {
                            merged.push(last);
                            merged.push(pm);
                        }
                    }
                }
                pattern_ops = merged;
            }
            let mut all = other_ops;
            all.extend(pattern_ops);
            if all.len() == 1 {
                if let Some(only) = all.pop() {
                    return only;
                }
            }
            return OperatorTree::Intersect(all);
        }
        map_operator_children(op, Self::fuse_join_pattern)
    }

    pub(super) fn merge_patterns(a: &OperatorTree, b: &OperatorTree) -> Option<OperatorTree> {
        let (
            OperatorTree::PatternMatch {
                pattern: pa,
                graph: ga,
            },
            OperatorTree::PatternMatch {
                pattern: pb,
                graph: _gb,
            },
        ) = (a, b)
        else {
            return None;
        };
        let vars_a: std::collections::BTreeSet<&String> =
            pa.vertex_patterns.iter().map(|vp| &vp.variable).collect();
        let vars_b: std::collections::BTreeSet<&String> =
            pb.vertex_patterns.iter().map(|vp| &vp.variable).collect();
        if vars_a.intersection(&vars_b).next().is_none() {
            return None;
        }
        // The merge buffer for the merge buffer, which keeps
        // insertion order. Mirror that with a parallel `Vec` (positions)
        // + `BTreeMap` (lookup) so structurally-equivalent inputs give
        // structurally-equivalent merged patterns regardless of name
        // collation.
        let mut order: Vec<String> = Vec::new();
        let mut by_var: std::collections::BTreeMap<String, VertexPatternIR> =
            std::collections::BTreeMap::new();
        for vp in &pa.vertex_patterns {
            order.push(vp.variable.clone());
            by_var.insert(vp.variable.clone(), vp.clone());
        }
        for vp in &pb.vertex_patterns {
            match by_var.get_mut(&vp.variable) {
                Some(existing) => {
                    let mut combined = existing.constraints.clone();
                    combined.extend(vp.constraints.iter().cloned());
                    existing.constraints = combined;
                }
                None => {
                    order.push(vp.variable.clone());
                    by_var.insert(vp.variable.clone(), vp.clone());
                }
            }
        }
        let merged_vps: Vec<VertexPatternIR> = order
            .into_iter()
            .filter_map(|var| by_var.remove(&var))
            .collect();
        let mut edge_patterns = pa.edge_patterns.clone();
        edge_patterns.extend(pb.edge_patterns.iter().cloned());
        let new_pattern = GraphPatternIR {
            vertex_patterns: merged_vps,
            edge_patterns,
        };
        Some(OperatorTree::PatternMatch {
            pattern: new_pattern,
            graph: ga.clone(),
        })
    }
}
