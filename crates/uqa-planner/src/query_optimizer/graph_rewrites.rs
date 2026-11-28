//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph-specific predicate pushdown and pattern fusion rewrites.

use std::{collections::HashMap, sync::Arc};

use uqa_operators::{EdgeConstraint, GraphPatternIR, OperatorTree, VertexPatternIR};

use super::{tree_map::map_operator_children, QueryOptimizer};

impl QueryOptimizer {
    // ---------------------------------------------------------------
    // 3. Push graph pattern filters
    // ---------------------------------------------------------------

    pub(super) fn push_graph_pattern_filters(&self, op: OperatorTree) -> OperatorTree {
        let OperatorTree::Filter {
            field,
            predicate,
            source: Some(mut child),
        } = op
        else {
            return self.recurse_graph_pattern(op);
        };
        let OperatorTree::PatternMatch { pattern, .. } = child.as_mut() else {
            return OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.push_graph_pattern_filters(*child))),
            };
        };
        if let Some((variable, property)) = field.split_once('.') {
            if let Some(vertex) = pattern
                .vertex_patterns
                .iter_mut()
                .find(|vertex| vertex.variable == variable)
            {
                let property = property.to_owned();
                vertex.constraints.push(Arc::new(move |vertex| {
                    vertex
                        .properties
                        .get(&property)
                        .is_some_and(|value| predicate.evaluate(Some(value)))
                }));
                return *child;
            }
            // The last matching source_target name selects the label. Every occurrence of that exact edge receives the predicate.
            if let Some(position) = pattern.edge_patterns.iter().rposition(|edge| {
                variable
                    .strip_prefix(edge.source_var.as_str())
                    .and_then(|suffix| suffix.strip_prefix('_'))
                    == Some(edge.target_var.as_str())
            }) {
                let property = property.to_owned();
                let constraint: EdgeConstraint = Arc::new(move |edge| {
                    edge.properties
                        .get(&property)
                        .is_some_and(|value| predicate.evaluate(Some(value)))
                });
                let (before, remaining) = pattern.edge_patterns.split_at_mut(position);
                let (selected, after) = remaining.split_first_mut().expect("selected edge");
                for edge in before.iter_mut().chain(after) {
                    if edge.source_var == selected.source_var
                        && edge.target_var == selected.target_var
                        && edge.label == selected.label
                    {
                        edge.constraints.push(Arc::clone(&constraint));
                    }
                }
                selected.constraints.push(constraint);
                return *child;
            }
        }
        OperatorTree::Filter {
            field,
            predicate,
            source: Some(child),
        }
    }

    pub(super) fn recurse_graph_pattern(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.push_graph_pattern_filters(child))
    }

    // ---------------------------------------------------------------
    // 4. Push filter into Traverse
    // ---------------------------------------------------------------

    pub(super) fn push_filter_into_traverse(&self, op: OperatorTree) -> OperatorTree {
        let OperatorTree::Filter {
            field,
            predicate,
            source: Some(mut child),
        } = op
        else {
            return self.recurse_traverse_filter(op);
        };
        let OperatorTree::Traverse {
            vertex_predicate, ..
        } = child.as_mut()
        else {
            return OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.push_filter_into_traverse(*child))),
            };
        };
        let previous = vertex_predicate.take();
        *vertex_predicate = Some(Arc::new(move |vertex| {
            previous.as_ref().is_none_or(|check| check(vertex))
                && vertex
                    .properties
                    .get(&field)
                    .is_some_and(|value| predicate.evaluate(Some(value)))
        }));
        *child
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
                    field,
                    predicate,
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
        let OperatorTree::Intersect(operands) = op else {
            return map_operator_children(op, Self::fuse_join_pattern);
        };
        let mut others = Vec::new();
        let mut patterns = Vec::new();
        for child in operands.into_iter().map(Self::fuse_join_pattern) {
            match child {
                OperatorTree::PatternMatch { mut pattern, graph } => {
                    if let Some(OperatorTree::PatternMatch {
                        pattern: previous, ..
                    }) = patterns.last_mut()
                    {
                        match merge_patterns(previous, pattern) {
                            Ok(()) => continue,
                            Err(unmerged) => pattern = unmerged,
                        }
                    }
                    patterns.push(OperatorTree::PatternMatch { pattern, graph });
                }
                other => others.push(other),
            }
        }
        others.extend(patterns);
        if others.len() == 1 {
            return others.pop().expect("single operand");
        }
        OperatorTree::Intersect(others)
    }
}

/// Preserve the first variable's position and the last declaration in the left pattern. Repeated right declarations append constraints in encounter order.
fn merge_patterns(left: &mut GraphPatternIR, right: GraphPatternIR) -> Result<(), GraphPatternIR> {
    let mut positions = HashMap::new();
    let mut slots = Vec::with_capacity(left.vertex_patterns.len() + right.vertex_patterns.len());
    for vertex in &left.vertex_patterns {
        let next = positions.len();
        slots.push(*positions.entry(vertex.variable.as_str()).or_insert(next));
    }
    let left_names = positions.len();
    let mut shared = false;
    for vertex in &right.vertex_patterns {
        let next = positions.len();
        let index = *positions.entry(vertex.variable.as_str()).or_insert(next);
        shared |= index < left_names;
        slots.push(index);
    }
    if !shared {
        return Err(right);
    }
    let mut vertices: Vec<VertexPatternIR> = Vec::with_capacity(positions.len());
    drop(positions);
    let mut slots = slots.into_iter();
    for vertex in left.vertex_patterns.drain(..) {
        let index = slots.next().expect("left variable position");
        if index == vertices.len() {
            vertices.push(vertex);
        } else {
            vertices[index] = vertex;
        }
    }
    for vertex in right.vertex_patterns {
        let index = slots.next().expect("right variable position");
        if index == vertices.len() {
            vertices.push(vertex);
        } else {
            vertices[index].constraints.extend(vertex.constraints);
        }
    }
    left.vertex_patterns = vertices;
    left.edge_patterns.extend(right.edge_patterns);
    Ok(())
}

#[cfg(test)]
mod tests;
