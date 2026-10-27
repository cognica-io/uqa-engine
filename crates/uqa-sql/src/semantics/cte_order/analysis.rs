//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependency indices share one admitted owner; lexical names remain borrowed from plans.

use super::references::{self, External, Selection};
use crate::{
    plan::{CommandPlan, CtePlan, QueryPlan},
    SQLError,
};
use std::cell::RefCell;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationName,
};

pub(crate) struct CteAnalysis<'p, 'c> {
    pub(crate) plans: Produced<Vec<&'p CtePlan>>,
    names: Produced<Vec<(RelationName<'p>, usize)>>,
    dependencies: Produced<Vec<Vec<usize>>>,
    primary: ProductionVec<'c, bool>,
    counts: ProductionVec<'c, u8>,
    control: ProductionControl<'c>,
}

fn flags(control: ProductionControl<'_>, len: usize) -> Result<ProductionVec<'_, bool>, SQLError> {
    let mut flags = ProductionVec::new(control);
    flags.reserve(len)?;
    for _ in 0..len {
        flags.push_copy(false)?;
    }
    Ok(flags)
}

impl<'p, 'c> CteAnalysis<'p, 'c> {
    pub(crate) fn new(
        plans: impl IntoIterator<Item = &'p CtePlan>,
        control: ProductionControl<'c>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut retained = ProductionVec::new(control);
        let mut names = ProductionVec::new(control);
        let mut counts = ProductionVec::new(control);
        for (index, plan) in plans.into_iter().enumerate() {
            retained.push_copy(plan)?;
            names.push_copy((RelationName::decoded(&plan.name), index))?;
            counts.push_copy(0u8)?;
        }
        names
            .as_mut_slice()
            .sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let plans = retained.finish()?;
        let names = names.finish()?;
        let mut dependencies = ProductionVec::new(control);
        let mut found = ProductionVec::new(control);
        for _ in plans.iter() {
            found.push_copy(0usize)?;
        }
        let found = RefCell::new(found);
        let counts = RefCell::new(counts);
        let recursive = plans.iter().any(|plan| plan.recursive);
        for (index, plan) in plans.iter().enumerate() {
            let selected = RefCell::new(ProductionVec::new(control));
            references::body(
                &plan.body,
                &|reference| {
                    control.check()?;
                    if let Some(target) = references::resolve(&names, reference) {
                        if found.borrow()[target] != index + 1 {
                            selected.borrow_mut().push_copy(target)?;
                            found.borrow_mut().as_mut_slice()[target] = index + 1;
                        }
                    }
                    Ok(())
                },
                control,
                Selection::Reachable,
            )?;
            let mut selected = selected.into_inner();
            selected.as_mut_slice().sort_unstable();
            dependencies.push_produced(selected.finish()?)?;
            let ceiling = if recursive { plans.len() } else { index };
            references::body(
                &plan.body,
                &|reference| {
                    control.check()?;
                    if let Some(index) =
                        references::resolve(&names, reference).filter(|index| *index < ceiling)
                    {
                        let mut counts = counts.borrow_mut();
                        let count = &mut counts.as_mut_slice()[index];
                        *count = count.saturating_add(1).min(2);
                    }
                    Ok(())
                },
                control,
                Selection::Syntactic,
            )?;
        }
        Ok(Self {
            primary: flags(control, plans.len())?,
            plans,
            names,
            dependencies: dependencies.finish()?,
            counts: counts.into_inner(),
            control,
        })
    }

    pub(crate) fn query(
        query: &'p QueryPlan,
        control: ProductionControl<'c>,
    ) -> Result<Self, SQLError> {
        let mut analysis = Self::new(&query.ctes, control)?;
        analysis.primary(|external, selection| {
            references::query_root(&query.root, external, control, selection)
        })?;
        Ok(analysis)
    }

    pub(crate) fn command(
        command: &'p CommandPlan,
        control: ProductionControl<'c>,
    ) -> Result<Self, SQLError> {
        let mut analysis = Self::new(command.ctes(), control)?;
        analysis.primary(|external, selection| {
            references::command_root(
                command.query_inputs(),
                command.source_input(),
                external,
                control,
                selection,
            )
        })?;
        Ok(analysis)
    }

    pub(crate) fn primary(
        &mut self,
        visit: impl Fn(&External<'_>, Selection) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        if self.plans.is_empty() {
            return Ok(());
        }
        let primary = RefCell::new(&mut self.primary);
        visit(
            &|reference| {
                self.control.check()?;
                if let Some(index) = references::resolve(&self.names, reference) {
                    primary.borrow_mut().as_mut_slice()[index] = true;
                }
                Ok(())
            },
            Selection::Reachable,
        )?;
        let counts = RefCell::new(&mut self.counts);
        visit(
            &|reference| {
                self.control.check()?;
                if let Some(index) = references::resolve(&self.names, reference) {
                    let mut counts = counts.borrow_mut();
                    let count = &mut counts.as_mut_slice()[index];
                    *count = count.saturating_add(1).min(2);
                }
                Ok(())
            },
            Selection::Syntactic,
        )
    }

    pub(crate) fn set_primary_names<'n>(
        &mut self,
        names: impl IntoIterator<Item = &'n str>,
    ) -> Result<(), SQLError> {
        for name in names {
            self.control.check()?;
            if let Some(index) = self.index(name) {
                self.primary.as_mut_slice()[index] = true;
            }
        }
        Ok(())
    }

    pub(crate) fn index(&self, name: &str) -> Option<usize> {
        self.names
            .binary_search_by(|(candidate, _)| candidate.cmp(&RelationName::decoded(name)))
            .ok()
            .map(|position| self.names[position].1)
    }

    pub(crate) fn primary_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.primary
            .iter()
            .enumerate()
            .filter_map(|(index, present)| present.then_some(index))
    }

    pub(crate) fn single_reference(&self, index: usize) -> bool {
        self.counts[index] == 1
    }

    pub(crate) fn recursive(&self, index: usize) -> bool {
        self.plans[index].recursive && self.dependencies[index].contains(&index)
    }

    pub(crate) fn dependencies(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        self.dependencies[index]
            .iter()
            .copied()
            .filter(move |target| self.plans[index].recursive || *target < index)
    }

    pub(crate) fn reachable(&self) -> Result<Produced<Vec<bool>>, SQLError> {
        self.closure(
            self.primary_indices().chain(
                self.plans
                    .iter()
                    .enumerate()
                    .filter_map(|(index, plan)| plan.body.modifies_data().then_some(index)),
            ),
        )
    }

    pub(crate) fn closure(
        &self,
        roots: impl IntoIterator<Item = usize>,
    ) -> Result<Produced<Vec<bool>>, SQLError> {
        let mut reached = flags(self.control, self.plans.len())?;
        let mut pending = ProductionVec::new(self.control);
        for index in roots {
            self.control.check()?;
            if !reached[index] {
                pending.push_copy(index)?;
                reached.as_mut_slice()[index] = true;
            }
        }
        let mut next = 0;
        while next < pending.len() {
            let index = pending[next];
            next += 1;
            for target in self.dependencies(index) {
                self.control.check()?;
                if !reached[target] {
                    pending.push_copy(target)?;
                    reached.as_mut_slice()[target] = true;
                }
            }
        }
        Ok(reached.finish()?)
    }

    /// Select the first ready original declaration, including definitions later omitted by reachability.
    pub(crate) fn order(&self) -> Result<Produced<Vec<usize>>, SQLError> {
        let mut ordered = ProductionVec::new(self.control);
        if !self.plans.iter().any(|plan| plan.recursive) {
            for index in 0..self.plans.len() {
                ordered.push_copy(index)?;
            }
            return Ok(ordered.finish()?);
        }
        super::dependencies::order(&self.dependencies, self.control)
    }
}
