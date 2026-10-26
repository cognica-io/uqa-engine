//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow nested lexical namespaces while reporting references to enclosing definitions.

use crate::{
    plan::{CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan},
    SQLError,
};
use std::cell::RefCell;
use uqa_core::{
    memory::{ProductionControl, ProductionVec},
    RelationIdentity, RelationName,
};

pub(in crate::semantics) type External<'a> = dyn Fn(&str) -> Result<(), SQLError> + 'a;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::semantics) enum Selection {
    Reachable,
    Syntactic,
}

struct Scope<'p, 'c, 'e> {
    definitions: &'p [CtePlan],
    names: ProductionVec<'c, (RelationName<'p>, usize)>,
    visited: RefCell<ProductionVec<'c, bool>>,
    pending: RefCell<ProductionVec<'c, usize>>,
    control: ProductionControl<'c>,
    external: &'e External<'e>,
    selection: Selection,
    all_visible: bool,
}

impl<'p, 'c, 'e> Scope<'p, 'c, 'e> {
    fn new(
        definitions: &'p [CtePlan],
        external: &'e External<'e>,
        control: ProductionControl<'c>,
        selection: Selection,
        query_scope: bool,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut visited = ProductionVec::new(control);
        let mut names = ProductionVec::new(control);
        for (index, definition) in definitions.iter().enumerate() {
            visited.push_copy(false)?;
            names.push_copy((RelationName::decoded(&definition.name), index))?;
        }
        names
            .as_mut_slice()
            .sort_unstable_by(|left, right| left.0.cmp(&right.0));
        Ok(Self {
            definitions,
            names,
            visited: RefCell::new(visited),
            pending: RefCell::new(ProductionVec::new(control)),
            control,
            external,
            selection,
            all_visible: (query_scope || selection == Selection::Syntactic)
                && definitions.iter().any(|cte| cte.recursive),
        })
    }

    fn reference(&self, reference: &str, ceiling: usize) -> Result<(), SQLError> {
        self.control.check()?;
        let selected = resolve(&self.names, reference).filter(|index| *index < ceiling);
        match selected {
            Some(index) if self.selection == Selection::Reachable => self.enqueue(index),
            Some(_) => Ok(()),
            None => (self.external)(reference),
        }
    }

    fn enqueue(&self, index: usize) -> Result<(), SQLError> {
        self.control.check()?;
        if self.visited.borrow()[index] {
            return Ok(());
        }
        // Reference discovery visits each body once. Dependency ordering owns cycle rejection.
        self.visited.borrow_mut().as_mut_slice()[index] = true;
        self.pending.borrow_mut().push_copy(index)?;
        Ok(())
    }

    fn definition(&self, index: usize) -> Result<(), SQLError> {
        self.control.check()?;
        let definition = &self.definitions[index];
        let recursive = definition.recursive || self.all_visible;
        let ceiling = if recursive {
            self.definitions.len()
        } else {
            index
        };
        body(
            &definition.body,
            &|reference| self.reference(reference, ceiling),
            self.control,
            self.selection,
        )
    }

    fn finish(self) -> Result<(), SQLError> {
        for (index, definition) in self.definitions.iter().enumerate() {
            self.control.check()?;
            if self.selection == Selection::Syntactic || definition.body.modifies_data() {
                self.enqueue(index)?;
            }
        }
        let mut next = 0;
        while next < self.pending.borrow().len() {
            let index = self.pending.borrow()[next];
            next += 1;
            self.definition(index)?;
        }
        Ok(())
    }
}

pub(in crate::semantics) fn resolve(
    names: &[(RelationName<'_>, usize)],
    reference: &str,
) -> Option<usize> {
    let reference = RelationIdentity::borrowed_reference(reference)?;
    if reference.schema.is_some() {
        return None;
    }
    names
        .binary_search_by(|(name, _)| name.cmp(&reference.name))
        .ok()
        .map(|position| names[position].1)
}

pub(in crate::semantics) fn body(
    body: &CtePlanBody,
    external: &External<'_>,
    control: ProductionControl<'_>,
    selection: Selection,
) -> Result<(), SQLError> {
    match body {
        CtePlanBody::Query(plan) => query(plan, external, control, selection),
        CtePlanBody::Command(plan) => command(plan, external, control, selection),
    }
}

pub(in crate::semantics) fn query(
    query: &QueryPlan,
    external: &External<'_>,
    control: ProductionControl<'_>,
    selection: Selection,
) -> Result<(), SQLError> {
    let scope = Scope::new(&query.ctes, external, control, selection, true)?;
    query_root(
        &query.root,
        &|name| scope.reference(name, query.ctes.len()),
        control,
        selection,
    )?;
    scope.finish()
}

fn command(
    command: &CommandPlan,
    external: &External<'_>,
    control: ProductionControl<'_>,
    selection: Selection,
) -> Result<(), SQLError> {
    let scope = Scope::new(command.ctes(), external, control, selection, false)?;
    command_root(
        command.query_inputs(),
        command.source_input(),
        &|name| scope.reference(name, command.ctes().len()),
        control,
        selection,
    )?;
    scope.finish()
}

pub(in crate::semantics) fn command_root<'p>(
    queries: impl IntoIterator<Item = &'p QueryPlan>,
    input: Option<&SourcePlan>,
    external: &External<'_>,
    control: ProductionControl<'_>,
    selection: Selection,
) -> Result<(), SQLError> {
    control.check()?;
    for plan in queries {
        query(plan, external, control, selection)?;
    }
    if let Some(input) = input {
        source(input, external, control, selection)?;
    }
    Ok(())
}

pub(in crate::semantics) fn query_root(
    root: &RelationalPlan,
    external: &External<'_>,
    control: ProductionControl<'_>,
    selection: Selection,
) -> Result<(), SQLError> {
    control.check()?;
    let subqueries = match root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(input) = &block.from {
                source(input, external, control, selection)?;
            }
            &block.subqueries
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            query(left, external, control, selection)?;
            query(right, external, control, selection)?;
            subqueries
        }
        RelationalPlan::Values { subqueries, .. } => subqueries,
    };
    for plan in subqueries {
        query(plan, external, control, selection)?;
    }
    Ok(())
}

fn source(
    source: &SourcePlan,
    external: &External<'_>,
    control: ProductionControl<'_>,
    selection: Selection,
) -> Result<(), SQLError> {
    control.check()?;
    match source {
        SourcePlan::Table { name, .. } => external(name)?,
        SourcePlan::Join { left, right, .. } => {
            self::source(left, external, control, selection)?;
            self::source(right, external, control, selection)?;
        }
        SourcePlan::Subquery { body, .. } => query(body, external, control, selection)?,
        SourcePlan::RetainedRows(_)
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
    Ok(())
}
