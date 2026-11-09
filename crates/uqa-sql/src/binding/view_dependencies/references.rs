//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed relation references retain lexical CTE visibility without copying stored plans.

use crate::{
    plan::{CommandPlan, CtePlan, CtePlanBody, PlanRef, RelationalPlan, SourcePlan},
    SQLError,
};
use uqa_core::{
    memory::{ProductionControl, ProductionMap, ProductionSmallVec},
    RelationIdentity, RelationName,
};

#[derive(Clone, Copy)]
enum Task<'a> {
    Plan(PlanRef<'a>),
    Root(&'a RelationalPlan),
    Source(&'a SourcePlan),
    Command(&'a CommandPlan),
    Ctes(&'a [CtePlan], usize, bool),
    Add(&'a str),
    Leave(&'a [CtePlan]),
}

/// Visit concrete sources and mutation targets in a stored query or SQL-standard routine body. Nonrecursive CTE bodies see outer and preceding names; recursive bodies see all siblings. Operator-join inputs and mutation targets always name real relations. The callback observes original names and inheritance selection without rebinding or evaluating anything. Scratch and cancellation use the supplied control; task order is not an execution order.
pub fn visit_stored_plan_relation_references(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str, bool) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let mut work = Work {
        pending: ProductionSmallVec::new(*control),
        visible: ProductionMap::new(*control),
        control: *control,
    };
    work.pending.push(Task::Plan(plan))?;
    while let Some(task) = work.pending.pop() {
        control.check()?;
        match task {
            Task::Plan(PlanRef::Query(query)) => {
                work.enter(&query.ctes, Task::Root(&query.root))?;
            }
            Task::Plan(PlanRef::Command(command)) => {
                work.enter(command.ctes(), Task::Command(command))?;
            }
            Task::Ctes(ctes, index, recursive) => {
                if let Some(cte) = ctes.get(index) {
                    work.pending.push(Task::Ctes(ctes, index + 1, recursive))?;
                    if !recursive {
                        work.pending.push(Task::Add(&cte.name))?;
                    }
                    work.pending.push(Task::Plan(match &cte.body {
                        CtePlanBody::Query(query) => PlanRef::Query(query),
                        CtePlanBody::Command(command) => PlanRef::Command(command),
                    }))?;
                }
            }
            Task::Add(name) => work.add(name)?,
            Task::Leave(ctes) => {
                for cte in ctes {
                    control.check()?;
                    let name = RelationName::decoded(&cte.name);
                    let count = work.visible.remove(&name).expect("entered CTE scope");
                    if count > 1 {
                        work.visible.insert_copy(name, count - 1)?;
                    }
                }
            }
            Task::Root(root) => work.root(root)?,
            Task::Source(source) => work.source(source, visit)?,
            Task::Command(command) => {
                if let Some(target) = command.mutation_target() {
                    let descendants = match command {
                        CommandPlan::Insert(plan) => plan.include_descendants,
                        CommandPlan::Update(plan) => plan.include_descendants,
                        CommandPlan::Delete(plan) => plan.include_descendants,
                        CommandPlan::Merge(plan) => plan.include_descendants,
                        _ => unreachable!("mutation target owner"),
                    };
                    visit(target, descendants)?;
                } else if !matches!(command, CommandPlan::Notify { .. } | CommandPlan::Empty) {
                    return Err(SQLError::Internal(
                        "unsupported stored command relation scope".into(),
                    ));
                }
                for query in command.query_inputs() {
                    work.pending.push(Task::Plan(PlanRef::Query(query)))?;
                }
                if let Some(source) = command.source_input() {
                    work.pending.push(Task::Source(source))?;
                }
            }
        }
    }
    control.check()?;
    Ok(())
}

struct Work<'a, 'c> {
    pending: ProductionSmallVec<'c, [Task<'a>; 8]>,
    visible: ProductionMap<'c, RelationName<'a>, usize>,
    control: ProductionControl<'c>,
}

impl<'a> Work<'a, '_> {
    fn add(&mut self, name: &'a str) -> Result<(), SQLError> {
        let name = RelationName::decoded(name);
        let count = self.visible.get(&name).copied().unwrap_or(0);
        self.visible.insert_copy(
            name,
            count
                .checked_add(1)
                .ok_or_else(|| SQLError::Internal("CTE scope depth overflow".into()))?,
        )?;
        Ok(())
    }

    fn enter(&mut self, ctes: &'a [CtePlan], body: Task<'a>) -> Result<(), SQLError> {
        let mut recursive = false;
        for cte in ctes {
            self.control.check()?;
            recursive |= cte.recursive;
        }
        if recursive {
            for cte in ctes {
                self.add(&cte.name)?;
            }
        }
        self.pending.push(Task::Leave(ctes))?;
        self.pending.push(body)?;
        self.pending.push(Task::Ctes(ctes, 0, recursive))?;
        Ok(())
    }

    fn root(&mut self, root: &'a RelationalPlan) -> Result<(), SQLError> {
        let subqueries = match root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &block.from {
                    self.pending.push(Task::Source(source))?;
                }
                &block.subqueries
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.pending.push(Task::Plan(PlanRef::Query(left)))?;
                self.pending.push(Task::Plan(PlanRef::Query(right)))?;
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for query in subqueries {
            self.pending.push(Task::Plan(PlanRef::Query(query)))?;
        }
        Ok(())
    }

    fn source(
        &mut self,
        source: &'a SourcePlan,
        visit: &mut impl FnMut(&str, bool) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        match source {
            SourcePlan::Table {
                name,
                include_descendants,
                ..
            } => {
                let reference = RelationIdentity::borrowed_reference(name).ok_or_else(|| {
                    SQLError::Internal("malformed stored relation reference".into())
                })?;
                if reference.schema.is_some() || self.visible.get(&reference.name).is_none() {
                    visit(name, *include_descendants)?;
                }
            }
            SourcePlan::Join { left, right, .. } => {
                self.pending.push(Task::Source(right))?;
                self.pending.push(Task::Source(left))?;
            }
            SourcePlan::Subquery { body, .. } => {
                self.pending.push(Task::Plan(PlanRef::Query(body)))?;
            }
            SourcePlan::Function { relations, .. } => {
                if let Some(relations) = relations {
                    visit(&relations.left, false)?;
                    self.control.check()?;
                    visit(&relations.right, false)?;
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    self.control.check()?;
                    if let Some(relations) = &function.relations {
                        visit(&relations.left, false)?;
                        self.control.check()?;
                        visit(&relations.right, false)?;
                    }
                }
            }
            SourcePlan::Values { .. } | SourcePlan::RetainedRows(_) => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
