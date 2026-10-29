//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable query traversal without moving the analyzed syntax nodes.

use super::{
    CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};

impl UnifiedPlan {
    pub fn visit_queries_mut(&mut self, visit: &mut dyn FnMut(&mut QueryPlan)) {
        self.try_visit_queries_mut(&mut |query| {
            visit(query);
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap_or_else(|never| match never {});
    }

    /// Visit command-owned query roots without descending into their query children.
    pub fn visit_query_roots_mut(&mut self, visit: &mut dyn FnMut(&mut QueryPlan)) {
        self.visit_queries_while_mut(&mut |query| {
            visit(query);
            Ok::<_, std::convert::Infallible>(false)
        })
        .unwrap_or_else(|never| match never {});
    }

    /// Visit query owners in pre-order, stopping immediately on the first error.
    pub fn try_visit_queries_mut<E>(
        &mut self,
        visit: &mut dyn FnMut(&mut QueryPlan) -> Result<(), E>,
    ) -> Result<(), E> {
        self.visit_queries_while_mut(&mut |query| visit(query).map(|()| true))
    }

    fn visit_queries_while_mut<E>(
        &mut self,
        visit: &mut dyn FnMut(&mut QueryPlan) -> Result<bool, E>,
    ) -> Result<(), E> {
        match self {
            Self::Query(query) => query.visit_queries_while_mut(visit),
            Self::Command(command) => visit_command(command, visit),
        }
    }
}

fn visit_ctes<E>(
    ctes: &mut [CtePlan],
    visit: &mut dyn FnMut(&mut QueryPlan) -> Result<bool, E>,
) -> Result<(), E> {
    for cte in ctes {
        match &mut cte.body {
            CtePlanBody::Query(query) => query.visit_queries_while_mut(visit)?,
            CtePlanBody::Command(command) => visit_command(command, visit)?,
        }
    }
    Ok(())
}

fn visit_command<E>(
    command: &mut CommandPlan,
    visit: &mut dyn FnMut(&mut QueryPlan) -> Result<bool, E>,
) -> Result<(), E> {
    match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            return query.visit_queries_while_mut(visit);
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            return body.visit_queries_while_mut(visit);
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                for query in &mut expression.subqueries {
                    query.visit_queries_while_mut(visit)?;
                }
            }
            return Ok(());
        }
        _ => {}
    }
    if let Some(ctes) = command.ctes_mut() {
        visit_ctes(ctes, visit)?;
    }
    for query in command.query_inputs_mut() {
        query.visit_queries_while_mut(visit)?;
    }
    if let Some(source) = command.source_input_mut() {
        visit_source(source, visit)?;
    }
    Ok(())
}

impl QueryPlan {
    pub fn visit_queries_mut(&mut self, visit: &mut dyn FnMut(&mut Self)) {
        self.try_visit_queries_mut(&mut |query| {
            visit(query);
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap_or_else(|never| match never {});
    }

    /// Visit this query and its children in ownership order, stopping immediately on error.
    pub fn try_visit_queries_mut<E>(
        &mut self,
        visit: &mut dyn FnMut(&mut Self) -> Result<(), E>,
    ) -> Result<(), E> {
        self.visit_queries_while_mut(&mut |query| visit(query).map(|()| true))
    }

    fn visit_queries_while_mut<E>(
        &mut self,
        visit: &mut dyn FnMut(&mut Self) -> Result<bool, E>,
    ) -> Result<(), E> {
        if !visit(self)? {
            return Ok(());
        }
        visit_ctes(&mut self.ctes, visit)?;
        let subqueries = match &mut self.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &mut block.from {
                    visit_source(source, visit)?;
                }
                &mut block.subqueries
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                left.visit_queries_while_mut(visit)?;
                right.visit_queries_while_mut(visit)?;
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for query in subqueries {
            query.visit_queries_while_mut(visit)?;
        }
        Ok(())
    }
}

fn visit_source<E>(
    source: &mut SourcePlan,
    visit: &mut dyn FnMut(&mut QueryPlan) -> Result<bool, E>,
) -> Result<(), E> {
    match source {
        SourcePlan::Subquery { body, .. } => body.visit_queries_while_mut(visit)?,
        SourcePlan::Table {
            executable_query: Some(body),
            ..
        } => body.visit_queries_while_mut(visit)?,
        SourcePlan::Join { left, right, .. } => {
            visit_source(left, visit)?;
            visit_source(right, visit)?;
        }
        _ => {}
    }
    Ok(())
}

impl CommandPlan {
    pub fn visit_query_roots_mut(&mut self, visit: &mut dyn FnMut(&mut QueryPlan)) {
        visit_command(self, &mut |query| {
            visit(query);
            Ok::<_, std::convert::Infallible>(false)
        })
        .unwrap_or_else(|never| match never {});
    }
}
