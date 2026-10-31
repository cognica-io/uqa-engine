//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consuming query visitation retains the existing ownership order and can stop at a query root.

use super::{
    CommandQuerySlot, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
    MutableSourcePlan, QueryChildSlot, SourceChildSlot,
};
use crate::{
    plan::{CommandPlan, RelationalPlan, SourcePlan},
    SQLError,
};
use uqa_core::memory::ProductionControl;

type Visitor<'a> = dyn FnMut(MutableQueryPlan) -> Result<(MutableQueryPlan, bool), SQLError> + 'a;

impl MutableExecutablePlan {
    /// Visit query owners in pre-order. The callback's boolean selects whether to recurse into the returned query's owned children.
    pub fn map_queries_while(
        self,
        control: &ProductionControl<'_>,
        visit: &mut Visitor<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        Ok(match self {
            Self::Query(query) => Self::Query(query.map_queries_while(control, visit)?),
            Self::Command(command) => Self::Command(command.map_queries_while(control, visit)?),
        })
    }
}

impl MutableQueryPlan {
    pub fn map_queries_while(
        self,
        control: &ProductionControl<'_>,
        visit: &mut Visitor<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (mut query, descend) = visit(self)?;
        query.assert_owner(control);
        control.check()?;
        if !descend {
            return Ok(query);
        }
        for index in 0..query.ctes.len() {
            query = query.map_cte(index, control, |body| {
                body.map_queries_while(control, visit)
            })?;
        }
        match &query.root {
            RelationalPlan::QueryBlock(_) => {
                query =
                    query.map_source(control, |source| source_queries(source, control, visit))?;
            }
            RelationalPlan::SetOp { .. } => {
                query = query.map_query_child(QueryChildSlot::Left, control, |query| {
                    query.map_queries_while(control, visit)
                })?;
                query = query.map_query_child(QueryChildSlot::Right, control, |query| {
                    query.map_queries_while(control, visit)
                })?;
            }
            RelationalPlan::Values { .. } => {}
        }
        let count = match &query.root {
            RelationalPlan::QueryBlock(block) => block.subqueries.len(),
            RelationalPlan::SetOp { subqueries, .. }
            | RelationalPlan::Values { subqueries, .. } => subqueries.len(),
        };
        for index in 0..count {
            query = query.map_query_child(QueryChildSlot::Subquery(index), control, |query| {
                query.map_queries_while(control, visit)
            })?;
        }
        Ok(query)
    }
}

impl MutableCommandPlan {
    pub fn map_queries_while(
        mut self,
        control: &ProductionControl<'_>,
        visit: &mut Visitor<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        match &*self {
            CommandPlan::CreateView { .. }
            | CommandPlan::CreateTableAs { .. }
            | CommandPlan::CreateMaterializedView { .. }
            | CommandPlan::DeclareCursor { .. } => {
                return self
                    .map_query_body(control, |query| query.map_queries_while(control, visit));
            }
            CommandPlan::Explain { .. } | CommandPlan::Prepare { .. } => {
                return self
                    .map_executable_body(control, |body| body.map_queries_while(control, visit));
            }
            CommandPlan::Call { args, .. } => {
                let count = args.len();
                for argument in 0..count {
                    let CommandPlan::Call { args, .. } = &*self else {
                        unreachable!()
                    };
                    for query in 0..args[argument].subqueries.len() {
                        self = self.map_query_child(
                            CommandQuerySlot::CallArgumentSubquery { argument, query },
                            control,
                            |query| query.map_queries_while(control, visit),
                        )?;
                    }
                }
                return Ok(self);
            }
            CommandPlan::Execute { params, .. } => {
                let count = params.len();
                for parameter in 0..count {
                    let CommandPlan::Execute { params, .. } = &*self else {
                        unreachable!()
                    };
                    for query in 0..params[parameter].subqueries.len() {
                        self = self.map_query_child(
                            CommandQuerySlot::ExecuteParameterSubquery { parameter, query },
                            control,
                            |query| query.map_queries_while(control, visit),
                        )?;
                    }
                }
                return Ok(self);
            }
            _ => {}
        }
        for index in 0..self.ctes().len() {
            self = self.map_cte(index, control, |body| {
                body.map_queries_while(control, visit)
            })?;
        }
        let (insert, count) = match &*self {
            CommandPlan::Insert(plan) => (plan.source.is_some(), plan.subqueries.len()),
            CommandPlan::Update(plan) => (false, plan.subqueries.len()),
            CommandPlan::Delete(plan) => (false, plan.subqueries.len()),
            CommandPlan::Merge(plan) => (false, plan.subqueries.len()),
            _ => (false, 0),
        };
        if insert {
            self = self.map_query_child(CommandQuerySlot::InsertSource, control, |query| {
                query.map_queries_while(control, visit)
            })?;
        }
        for index in 0..count {
            self = self.map_query_child(CommandQuerySlot::Subquery(index), control, |query| {
                query.map_queries_while(control, visit)
            })?;
        }
        self.map_source(control, |source| source_queries(source, control, visit))
    }
}

fn source_queries(
    mut source: MutableSourcePlan,
    control: &ProductionControl<'_>,
    visit: &mut Visitor<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    match &*source {
        SourcePlan::Subquery { .. }
        | SourcePlan::Table {
            executable_query: Some(_),
            ..
        } => source.map_query(control, |query| query.map_queries_while(control, visit)),
        SourcePlan::Join { .. } => {
            source = source.map_child(SourceChildSlot::Left, control, |source| {
                source_queries(source, control, visit)
            })?;
            source.map_child(SourceChildSlot::Right, control, |source| {
                source_queries(source, control, visit)
            })
        }
        _ => Ok(source),
    }
}
