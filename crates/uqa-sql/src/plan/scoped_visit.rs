//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable plan traversal borrows lexical CTE names without copying definitions or names.

use super::{
    CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use crate::SQLError;

/// Hooks receive the enclosing CTE namespace, using decoded, unqualified names. A command's own definitions are not enclosing names. Returning false leaves its children unvisited.
pub trait ScopedPlanVisitor {
    fn command(
        &mut self,
        _command: &mut CommandPlan,
        _outer: &dyn Fn(&str) -> bool,
    ) -> Result<bool, SQLError> {
        Ok(true)
    }

    fn source(
        &mut self,
        _source: &mut SourcePlan,
        _visible: &dyn Fn(&str) -> bool,
    ) -> Result<bool, SQLError> {
        Ok(true)
    }
}

impl UnifiedPlan {
    pub fn visit_scoped_mut(
        &mut self,
        visitor: &mut dyn ScopedPlanVisitor,
    ) -> Result<(), SQLError> {
        visit_plan(self, &|_| false, visitor)
    }
}

fn visit_plan(
    plan: &mut UnifiedPlan,
    outer: &dyn Fn(&str) -> bool,
    visitor: &mut dyn ScopedPlanVisitor,
) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => visit_query(query, outer, visitor),
        UnifiedPlan::Command(command) => visit_command(command, outer, visitor),
    }
}

fn visit_ctes(
    ctes: &mut [CtePlan],
    outer: &dyn Fn(&str) -> bool,
    visitor: &mut dyn ScopedPlanVisitor,
) -> Result<(), SQLError> {
    let recursive = ctes.iter().any(|cte| cte.recursive);
    for index in 0..ctes.len() {
        let (before, rest) = ctes.split_at_mut(index);
        let (cte, after) = rest.split_first_mut().expect("current CTE");
        let visible = |name: &str| {
            before.iter().any(|cte| cte.name == name)
                || (recursive && (cte.name == name || after.iter().any(|cte| cte.name == name)))
                || outer(name)
        };
        match &mut cte.body {
            CtePlanBody::Query(query) => visit_query(query, &visible, visitor)?,
            CtePlanBody::Command(command) => visit_command(command, &visible, visitor)?,
        }
    }
    Ok(())
}

fn visit_query(
    query: &mut QueryPlan,
    outer: &dyn Fn(&str) -> bool,
    visitor: &mut dyn ScopedPlanVisitor,
) -> Result<(), SQLError> {
    visit_ctes(&mut query.ctes, outer, visitor)?;
    let visible = |name: &str| query.ctes.iter().any(|cte| cte.name == name) || outer(name);
    let subqueries = match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                visit_source(source, &visible, visitor)?;
            }
            &mut block.subqueries
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            visit_query(left, &visible, visitor)?;
            visit_query(right, &visible, visitor)?;
            subqueries
        }
        RelationalPlan::Values { subqueries, .. } => subqueries,
    };
    for query in subqueries {
        visit_query(query, &visible, visitor)?;
    }
    Ok(())
}

fn visit_source(
    source: &mut SourcePlan,
    visible: &dyn Fn(&str) -> bool,
    visitor: &mut dyn ScopedPlanVisitor,
) -> Result<(), SQLError> {
    if !visitor.source(source, visible)? {
        return Ok(());
    }
    match source {
        SourcePlan::Subquery { body, .. } => visit_query(body, visible, visitor)?,
        SourcePlan::Table {
            name,
            executable_query: Some(body),
            ..
        } => {
            if crate::semantics::cte_reference_name(name).is_some_and(|name| visible(&name)) {
                visit_query(body, visible, visitor)?;
            } else {
                // Stored view definitions have their own namespace, independently of the invoking query's WITH names.
                visit_query(body, &|_| false, visitor)?;
            }
        }
        SourcePlan::Join { left, right, .. } => {
            visit_source(left, visible, visitor)?;
            visit_source(right, visible, visitor)?;
        }
        _ => {}
    }
    Ok(())
}

fn visit_command(
    command: &mut CommandPlan,
    outer: &dyn Fn(&str) -> bool,
    visitor: &mut dyn ScopedPlanVisitor,
) -> Result<(), SQLError> {
    if !visitor.command(command, outer)? {
        return Ok(());
    }
    let (ctes, source, query, subqueries) = match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => return visit_query(query, outer, visitor),
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            return visit_plan(body, outer, visitor)
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                for query in &mut expression.subqueries {
                    visit_query(query, outer, visitor)?;
                }
            }
            return Ok(());
        }
        CommandPlan::Insert(plan) => (
            &mut plan.ctes,
            None,
            plan.source.as_deref_mut(),
            &mut plan.subqueries,
        ),
        CommandPlan::Update(plan) => (
            &mut plan.ctes,
            plan.source.as_deref_mut(),
            None,
            &mut plan.subqueries,
        ),
        CommandPlan::Delete(plan) => (
            &mut plan.ctes,
            plan.source.as_deref_mut(),
            None,
            &mut plan.subqueries,
        ),
        CommandPlan::Merge(plan) => (
            &mut plan.ctes,
            Some(plan.source.as_mut()),
            None,
            &mut plan.subqueries,
        ),
        _ => return Ok(()),
    };
    visit_ctes(ctes, outer, visitor)?;
    let visible = |name: &str| ctes.iter().any(|cte| cte.name == name) || outer(name);
    if let Some(source) = source {
        visit_source(source, &visible, visitor)?;
    }
    if let Some(query) = query {
        visit_query(query, &visible, visitor)?;
    }
    for query in subqueries {
        visit_query(query, &visible, visitor)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
