//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Share node scans across independent input bindings without retaining a graph payload cache.

use super::{
    agtype, pattern_variables, BTreeSet, Binding, BindingRow, CypherError, CypherExecutor,
    CypherExpr, GraphStore, MatchClause, PathElement, Value,
};

impl<G: GraphStore> CypherExecutor<'_, G> {
    pub(super) fn try_node_match_batch(
        &self,
        clause: &MatchClause,
        bindings: &[BindingRow],
    ) -> Result<Option<Vec<BindingRow>>, CypherError> {
        if bindings.len() < 2 || clause.r#where.is_some() {
            return Ok(None);
        }
        let mut nodes = Vec::new();
        let mut variables = BTreeSet::new();
        for pattern in &clause.patterns {
            let [PathElement::Node(node)] = pattern.elements.as_slice() else {
                return Ok(None);
            };
            if pattern.variable.is_some()
                || node.variable.as_ref().is_some_and(|variable| {
                    variables.contains(variable)
                        || bindings.iter().any(|row| row.contains_key(variable))
                })
                || node.properties.as_ref().is_some_and(|properties| {
                    properties.values().any(|expression| {
                        !self.batch_property_is_infallible(expression, bindings, &variables)
                    })
                })
            {
                return Ok(None);
            }
            if let Some(variable) = &node.variable {
                variables.insert(variable.clone());
            }
            nodes.push(node);
        }

        let mut groups: Vec<Vec<BindingRow>> =
            bindings.iter().cloned().map(|row| vec![row]).collect();
        for node in nodes {
            let seeds: Vec<&BindingRow> = groups.iter().flatten().collect();
            if seeds.is_empty() {
                break;
            }
            let mut matched = vec![Vec::new(); seeds.len()];
            let candidates = self.node_candidate_ids(node)?;
            self.for_each_node_vertex(node, &candidates, |vertex| {
                for (index, seed) in seeds.iter().enumerate() {
                    if self.node_matches(node, vertex, seed)? {
                        // The ordinary matcher validates the agtype graph ID even without a path variable.
                        if i64::try_from(vertex.vertex_id).is_err() {
                            agtype::vertex_to_value(vertex)?;
                        }
                        let mut row = (*seed).clone();
                        if let Some(variable) = &node.variable {
                            row.insert(variable.clone(), Binding::Vertex(vertex.clone()));
                        }
                        matched[index].push(row);
                    }
                }
                Ok(())
            })?;
            // Preserve the original seed-major order and every duplicate match.
            let mut matched = matched.into_iter();
            groups = groups
                .into_iter()
                .map(|group| {
                    group
                        .into_iter()
                        .flat_map(|_| matched.next().expect("one result group per seed"))
                        .collect()
                })
                .collect();
        }
        let mut output = Vec::new();
        for (original, matches) in bindings.iter().zip(groups) {
            if matches.is_empty() && clause.optional {
                let mut padded = original.clone();
                for variable in pattern_variables(&clause.patterns) {
                    padded
                        .entry(variable)
                        .or_insert(Binding::Value(Value::Null));
                }
                output.push(padded);
            } else {
                output.extend(matches);
            }
        }
        Ok(Some(output))
    }

    fn batch_property_is_infallible(
        &self,
        expression: &CypherExpr,
        bindings: &[BindingRow],
        previous_variables: &BTreeSet<String>,
    ) -> bool {
        match expression {
            CypherExpr::Literal(_) => true,
            CypherExpr::Parameter(parameter) => self.params.contains_key(&parameter.name),
            CypherExpr::Variable(variable) => bindings
                .iter()
                .all(|row| matches!(row.get(&variable.name), Some(Binding::Value(_)))),
            CypherExpr::PropertyAccess(property) if property.keys.len() == 1 => {
                previous_variables.contains(&property.variable)
                    || bindings.iter().all(|row| {
                        matches!(
                            row.get(&property.variable),
                            Some(
                                Binding::Vertex(_)
                                    | Binding::Edge(_)
                                    | Binding::Value(Value::Map(_) | Value::Null)
                            )
                        )
                    })
            }
            _ => false,
        }
    }
}
