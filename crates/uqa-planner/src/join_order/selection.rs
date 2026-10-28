//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    JoinGraph, JoinGraphError, JoinGraphResult, JoinOrderError, JoinOrderOptimizer, JoinPlan,
    JoinPredicate, JoinRelationRef, Produced, ProductionControl,
};
use crate::join_enumerator::enumerate_dpccp_with_control;
use uqa_core::memory::ProductionVec;

pub(super) struct Selection<'a> {
    pub plan: Produced<JoinPlan>,
    pub aliases: Aliases<'a>,
}

/// Relation identifiers have the graph's fixed 64-entry domain. Borrowed
/// names and sorted slots need neither heap storage nor copied statistics.
pub(super) struct Aliases<'a> {
    entries: [(&'a str, usize); 64],
    len: usize,
}

impl<'a> Aliases<'a> {
    fn new() -> Self {
        Self {
            entries: [("", 0); 64],
            len: 0,
        }
    }

    fn insert(&mut self, alias: &'a str, index: usize) -> JoinGraphResult<()> {
        match self.entries[..self.len].binary_search_by(|(name, _)| name.cmp(&alias)) {
            Ok(_) => Err(JoinGraphError::DuplicateAlias {
                alias: alias.to_owned(),
            }),
            Err(position) => {
                self.entries.copy_within(position..self.len, position + 1);
                self.entries[position] = (alias, index);
                self.len += 1;
                Ok(())
            }
        }
    }

    pub fn index(&self, alias: &str) -> JoinGraphResult<usize> {
        self.entries[..self.len]
            .binary_search_by(|(name, _)| name.cmp(&alias))
            .map(|position| self.entries[position].1)
            .map_err(|_| JoinGraphError::UnknownAlias {
                alias: alias.to_owned(),
            })
    }
}

impl JoinOrderOptimizer {
    pub(super) fn select<'a>(
        &self,
        relations: impl ExactSizeIterator<Item = JoinRelationRef<'a>> + Clone,
        predicates: &[JoinPredicate],
        control: &ProductionControl<'_>,
    ) -> Result<Selection<'a>, JoinOrderError> {
        control.check()?;
        if relations.len() == 0 {
            return Err(JoinGraphError::EmptyGraph.into());
        }

        let mut names = ProductionVec::new(*control);
        let mut cardinalities = ProductionVec::new(*control);
        let mut access_costs = ProductionVec::new(*control);
        let mut edges = ProductionVec::new(*control);
        let mut aliases = Aliases::new();
        for (index, relation) in relations.clone().enumerate() {
            let (name, memory) = control.copy_text(relation.alias)?.into_parts();
            let name = JoinGraph::validate_relation(
                index,
                name,
                relation.cardinality,
                relation.access_cost,
            )?;
            names.push_produced(control.finish(name, memory)?)?;
            cardinalities.push_copy(relation.cardinality)?;
            access_costs.push_copy(relation.access_cost)?;
            aliases.insert(relation.alias, index)?;
        }

        // The established singleton interface validates the relation but does
        // not inspect predicates that cannot connect it to another relation.
        if relations.len() > 1 {
            for predicate in predicates {
                control.check()?;
                let left = aliases.index(&predicate.left_alias)?;
                let right = aliases.index(&predicate.right_alias)?;
                let selectivity = Self::estimate_predicate_selectivity(
                    relations.clone(),
                    left,
                    right,
                    &predicate.left_field,
                    &predicate.right_field,
                );
                edges.push_copy(JoinGraph::validated_edge(
                    relations.len(),
                    left,
                    right,
                    selectivity,
                )?)?;
            }
        }

        let names = names.finish()?;
        let cardinalities = cardinalities.finish()?;
        let access_costs = access_costs.finish()?;
        let edges = edges.finish()?;
        let (names, names_memory) = names.into_parts();
        let (cardinalities, cardinalities_memory) = cardinalities.into_parts();
        let (access_costs, access_memory) = access_costs.into_parts();
        let (edges, edges_memory) = edges.into_parts();
        let graph = control.finish(
            JoinGraph {
                relations: names,
                cardinalities,
                access_costs,
                edges,
            },
            control.combine(
                control.combine(names_memory, cardinalities_memory),
                control.combine(access_memory, edges_memory),
            ),
        )?;
        let plan = enumerate_dpccp_with_control(&graph, self.cost_estimator.clone(), control)?
            .ok_or(JoinGraphError::EmptyGraph)?;
        Ok(Selection { plan, aliases })
    }
}
