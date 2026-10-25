//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select reachable WITH definitions and their dependency order using admitted, borrowed scratch.

#[cfg(test)]
mod tests;

use crate::{
    plan::{CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan},
    SQLError,
};
use std::cell::RefCell;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationName,
};

type External<'a> = dyn Fn(&str) -> Result<(), SQLError> + 'a;

/// Reachable declaration indices in lexical/dependency order. Unread query definitions are omitted; data-modifying definitions are always included.
pub fn reachable_query_cte_order_with_control(
    query: &QueryPlan,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    if query.ctes.is_empty() {
        return Ok(ProductionVec::new(control).finish()?);
    }
    query_scope(query, &|_| Ok(()), control)
}

/// Apply the same reachability and dependency rules to a command's WITH list.
pub fn reachable_command_cte_order_with_control(
    command: &CommandPlan,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    if command.ctes().is_empty() {
        return Ok(ProductionVec::new(control).finish()?);
    }
    command_scope(command, &|_| Ok(()), control)
}

struct Scope<'p, 'c, 'e> {
    definitions: &'p [CtePlan],
    names: ProductionVec<'c, (RelationName<'p>, usize)>,
    states: RefCell<ProductionVec<'c, u8>>,
    ordered: RefCell<ProductionVec<'c, usize>>,
    control: ProductionControl<'c>,
    external: &'e External<'e>,
}

impl<'p, 'c, 'e> Scope<'p, 'c, 'e> {
    fn new(
        definitions: &'p [CtePlan],
        external: &'e External<'e>,
        control: ProductionControl<'c>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut states = ProductionVec::new(control);
        states.reserve(definitions.len())?;
        let mut names = ProductionVec::new(control);
        for (index, definition) in definitions.iter().enumerate() {
            states.push_copy(0)?;
            names.push_copy((RelationName::decoded(&definition.name), index))?;
        }
        names
            .as_mut_slice()
            .sort_unstable_by(|left, right| left.0.cmp(&right.0));
        Ok(Self {
            definitions,
            names,
            states: RefCell::new(states),
            ordered: RefCell::new(ProductionVec::new(control)),
            control,
            external,
        })
    }

    fn reference(
        &self,
        reference: &str,
        ceiling: usize,
        defining: Option<usize>,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        let selected = uqa_core::RelationIdentity::borrowed_reference(reference)
            .filter(|reference| reference.schema.is_none())
            .and_then(|reference| {
                self.names
                    .binary_search_by(|(name, _)| name.cmp(&reference.name))
                    .ok()
            })
            .map(|position| self.names[position].1)
            .filter(|index| *index < ceiling);
        match selected {
            Some(index) => self.definition(index, defining),
            None => (self.external)(reference),
        }
    }

    fn definition(&self, index: usize, defining: Option<usize>) -> Result<(), SQLError> {
        self.control.check()?;
        let state = self.states.borrow()[index];
        match state {
            2 => return Ok(()),
            1 if defining == Some(index) => return Ok(()),
            1 => {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "mutual recursion between WITH items is not implemented".into(),
                })
            }
            _ => {}
        }
        self.states.borrow_mut().as_mut_slice()[index] = 1;
        let definition = &self.definitions[index];
        let ceiling = if definition.recursive {
            self.definitions.len()
        } else {
            index
        };
        let external = |reference: &str| self.reference(reference, ceiling, Some(index));
        match &definition.body {
            CtePlanBody::Query(query) => {
                query_scope(query, &external, self.control)?;
            }
            CtePlanBody::Command(command) => {
                command_scope(command, &external, self.control)?;
            }
        }
        self.ordered.borrow_mut().push_copy(index)?;
        self.states.borrow_mut().as_mut_slice()[index] = 2;
        Ok(())
    }

    fn finish(self) -> Result<Produced<Vec<usize>>, SQLError> {
        for (index, definition) in self.definitions.iter().enumerate() {
            if definition.body.modifies_data() {
                self.definition(index, None)?;
            }
        }
        let mut ordered = self.ordered.into_inner();
        // Nonrecursive definitions see only earlier siblings, even when the primary query names them in another order.
        if !self
            .definitions
            .iter()
            .any(|definition| definition.recursive)
        {
            ordered.as_mut_slice().sort_unstable();
        }
        Ok(ordered.finish()?)
    }
}

fn query_scope(
    query: &QueryPlan,
    external: &External<'_>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    let scope = Scope::new(&query.ctes, external, control)?;
    query_root(
        &query.root,
        &|name| scope.reference(name, query.ctes.len(), None),
        control,
    )?;
    scope.finish()
}

fn command_scope(
    command: &CommandPlan,
    external: &External<'_>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    let scope = Scope::new(command.ctes(), external, control)?;
    let reference = |name: &str| scope.reference(name, command.ctes().len(), None);
    if let CommandPlan::Insert(insert) = command {
        if let Some(query) = &insert.source {
            query_scope(query, &reference, control)?;
        }
    }
    for query in command.scalar_subqueries() {
        query_scope(query, &reference, control)?;
    }
    if let Some(source) = command.source_input() {
        source_references(source, &reference, control)?;
    }
    scope.finish()
}

fn query_root(
    root: &RelationalPlan,
    external: &External<'_>,
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let subqueries = match root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                source_references(source, external, control)?;
            }
            &block.subqueries
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            query_scope(left, external, control)?;
            query_scope(right, external, control)?;
            subqueries
        }
        RelationalPlan::Values { subqueries, .. } => subqueries,
    };
    for query in subqueries {
        query_scope(query, external, control)?;
    }
    Ok(())
}

fn source_references(
    source: &SourcePlan,
    external: &External<'_>,
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match source {
        SourcePlan::Table { name, .. } => external(name)?,
        SourcePlan::Join { left, right, .. } => {
            source_references(left, external, control)?;
            source_references(right, external, control)?;
        }
        SourcePlan::Subquery { body, .. } => {
            query_scope(body, external, control)?;
        }
        SourcePlan::RetainedRows(_)
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
    Ok(())
}
