//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lexical demand frames retain admitted names independently of mutable CTE bodies.

use super::{Demand, OptimizerConfig, QueryPlan};
use crate::CtePlan;
use std::cell::{Cell, RefCell};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionMap, ProductionVec},
    RelationName,
};
use uqa_sql::SQLError;

pub(super) struct CteDemand<'c> {
    name: Produced<String>,
    pub positions: RefCell<Option<Demand<'c>>>,
    pub observed: Cell<bool>,
    pub visible: Cell<bool>,
}

pub(super) struct Scope<'s, 'c> {
    pub parent: Option<&'s Scope<'s, 'c>>,
    entries: Produced<Vec<CteDemand<'c>>>,
}

impl<'s, 'c> Scope<'s, 'c> {
    pub fn query(
        query: &QueryPlan,
        config: &OptimizerConfig<'c>,
        parent: Option<&'s Self>,
    ) -> Result<(Self, Produced<Vec<usize>>), SQLError> {
        let control = config.production;
        let mut folded = ProductionMap::new(control);
        if let Some(context) = &config.routine_inlining {
            let scheduled = uqa_sql::semantics::cte_strategy::schedule_plan_ctes_with_control(
                context.volatility,
                query,
                control,
            )?;
            for cte in scheduled.iter() {
                control.check()?;
                if cte.deferred {
                    folded.insert_copy(cte.plan.name.as_str(), ())?;
                }
            }
        }
        let names = names(&query.ctes, control)?;
        let scope = Self::capture(&names, &folded, parent, control)?;
        drop(folded);
        let ordered = uqa_sql::semantics::order_cte_plans_with_control(&query.ctes, control)?;
        let mut indices = ProductionVec::new(control);
        indices.reserve(ordered.len())?;
        for cte in ordered.iter() {
            indices.push_copy(*names.get(cte.name.as_str()).expect("declared CTE"))?;
        }
        Ok((scope, indices.finish()?))
    }

    pub fn command(
        ctes: &[CtePlan],
        control: ProductionControl<'c>,
        parent: Option<&'s Self>,
    ) -> Result<Self, SQLError> {
        let names = names(ctes, control)?;
        Self::capture(&names, &ProductionMap::new(control), parent, control)
    }

    fn capture(
        names: &ProductionMap<'_, &str, usize>,
        folded: &ProductionMap<'_, &str, ()>,
        parent: Option<&'s Self>,
        control: ProductionControl<'c>,
    ) -> Result<Self, SQLError> {
        let mut entries = ProductionVec::new(control);
        entries.reserve(names.len())?;
        for (name, _) in names {
            let entry = CteDemand {
                name: control.copy_text(name)?,
                positions: RefCell::new(folded.get(*name).map(|()| Demand::new(control))),
                observed: Cell::new(false),
                visible: Cell::new(true),
            };
            entries.push_produced(control.finish(entry, control.empty_reservation())?)?;
        }
        Ok(Self {
            parent,
            entries: entries.finish()?,
        })
    }

    pub fn local(&self, name: &str) -> &CteDemand<'c> {
        &self.entries[self
            .entries
            .binary_search_by(|entry| entry.name.as_str().cmp(name))
            .expect("declared CTE")]
    }

    pub fn find(&self, name: RelationName<'_>) -> Option<&CteDemand<'c>> {
        let mut current = Some(self);
        while let Some(scope) = current {
            if let Ok(index) = scope
                .entries
                .binary_search_by(|entry| RelationName::decoded(&entry.name).cmp(&name))
            {
                let entry = &scope.entries[index];
                if entry.visible.get() {
                    return Some(entry);
                }
            }
            current = scope.parent;
        }
        None
    }

    pub fn set_visible(
        &self,
        ctes: &[CtePlan],
        before: usize,
        recursive: bool,
        control: ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        for (index, cte) in ctes.iter().enumerate() {
            control.check()?;
            self.local(&cte.name)
                .visible
                .set(recursive || index < before);
        }
        Ok(())
    }

    pub fn retain_visible_values(&self, control: ProductionControl<'_>) -> Result<(), SQLError> {
        let mut current = Some(self);
        while let Some(scope) = current {
            for demand in scope.entries.iter() {
                control.check()?;
                if demand.visible.get() {
                    *demand.positions.borrow_mut() = None;
                }
            }
            current = scope.parent;
        }
        Ok(())
    }
}

fn names<'p, 'c>(
    ctes: &'p [CtePlan],
    control: ProductionControl<'c>,
) -> Result<ProductionMap<'c, &'p str, usize>, SQLError> {
    let mut names = ProductionMap::new(control);
    for (index, cte) in ctes.iter().enumerate() {
        names.insert_copy(cte.name.as_str(), index)?;
    }
    Ok(names)
}
