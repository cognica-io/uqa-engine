//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command demand retains CTE visibility and visits each owned query arena once.

use super::{ctes, Context, SQLError};
use crate::CommandPlan;
use uqa_sql::plan::{CommandQuerySlot, MutableCommandPlan};

impl Context<'_, '_> {
    pub(super) fn command(
        &self,
        mut command: MutableCommandPlan,
    ) -> Result<MutableCommandPlan, SQLError> {
        let control = &self.config.production;
        if !matches!(
            &*command,
            CommandPlan::Insert(_)
                | CommandPlan::Update(_)
                | CommandPlan::Delete(_)
                | CommandPlan::Merge(_)
        ) {
            return self.utility(command);
        }
        let scope = if command.ctes().is_empty() {
            None
        } else {
            Some(ctes::Scope::command(command.ctes(), *control, self.ctes)?)
        };
        let context = Context {
            ctes: scope.as_ref().or(self.ctes),
            ..*self
        };
        for index in 0..command.ctes().len() {
            control.check()?;
            context.ctes.expect("command CTE scope").set_visible(
                command.ctes(),
                index,
                command.ctes()[index].recursive,
                *control,
            )?;
            command = command.map_cte(index, &self.storage, |body| context.statement(body))?;
        }
        if let Some(scope) = &scope {
            scope.set_visible(command.ctes(), command.ctes().len(), true, *control)?;
        }
        let (insert, count) = match &*command {
            CommandPlan::Insert(plan) => (plan.source.is_some(), plan.subqueries.len()),
            CommandPlan::Update(plan) => (false, plan.subqueries.len()),
            CommandPlan::Delete(plan) => (false, plan.subqueries.len()),
            CommandPlan::Merge(plan) => (false, plan.subqueries.len()),
            _ => unreachable!(),
        };
        if insert {
            command = command.map_query_child(
                CommandQuerySlot::InsertSource,
                &self.storage,
                |query| context.query(query),
            )?;
        }
        for index in 0..count {
            command = command.map_query_child(
                CommandQuerySlot::Subquery(index),
                &self.storage,
                |query| context.query(query),
            )?;
        }
        command.map_source(&self.storage, |source| context.source_queries(source))
    }

    fn utility(&self, mut command: MutableCommandPlan) -> Result<MutableCommandPlan, SQLError> {
        let control = &self.storage;
        match &*command {
            CommandPlan::CreateView { .. }
            | CommandPlan::CreateTableAs { .. }
            | CommandPlan::CreateMaterializedView { .. }
            | CommandPlan::DeclareCursor { .. } => {
                command.map_query_body(control, |query| self.query(query))
            }
            CommandPlan::Explain { .. } | CommandPlan::Prepare { .. } => {
                command.map_executable_body(control, |body| self.statement(body))
            }
            CommandPlan::Call { args, .. } => {
                let count = args.len();
                for argument in 0..count {
                    let CommandPlan::Call { args, .. } = &*command else {
                        unreachable!()
                    };
                    for query in 0..args[argument].subqueries.len() {
                        command = command.map_query_child(
                            CommandQuerySlot::CallArgumentSubquery { argument, query },
                            control,
                            |query| self.query(query),
                        )?;
                    }
                }
                Ok(command)
            }
            CommandPlan::Execute { params, .. } => {
                let count = params.len();
                for parameter in 0..count {
                    let CommandPlan::Execute { params, .. } = &*command else {
                        unreachable!()
                    };
                    for query in 0..params[parameter].subqueries.len() {
                        command = command.map_query_child(
                            CommandQuerySlot::ExecuteParameterSubquery { parameter, query },
                            control,
                            |query| self.query(query),
                        )?;
                    }
                }
                Ok(command)
            }
            _ => Ok(command),
        }
    }
}
