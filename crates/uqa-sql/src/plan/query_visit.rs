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
        match self {
            Self::Query(query) => query.visit_queries_mut(visit),
            Self::Command(command) => visit_command(command, visit),
        }
    }
}

fn visit_ctes(ctes: &mut [CtePlan], visit: &mut dyn FnMut(&mut QueryPlan)) {
    for cte in ctes {
        match &mut cte.body {
            CtePlanBody::Query(query) => query.visit_queries_mut(visit),
            CtePlanBody::Command(command) => visit_command(command, visit),
        }
    }
}

fn visit_command(command: &mut CommandPlan, visit: &mut dyn FnMut(&mut QueryPlan)) {
    match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            query.visit_queries_mut(visit);
            return;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            body.visit_queries_mut(visit);
            return;
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                for query in &mut expression.subqueries {
                    query.visit_queries_mut(visit);
                }
            }
            return;
        }
        _ => {}
    }
    if let Some(ctes) = command.ctes_mut() {
        visit_ctes(ctes, visit);
    }
    for query in command.query_inputs_mut() {
        query.visit_queries_mut(visit);
    }
    if let Some(source) = command.source_input_mut() {
        visit_source(source, visit);
    }
}

impl QueryPlan {
    pub fn visit_queries_mut(&mut self, visit: &mut dyn FnMut(&mut Self)) {
        visit(self);
        visit_ctes(&mut self.ctes, visit);
        let subqueries = match &mut self.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &mut block.from {
                    visit_source(source, visit);
                }
                &mut block.subqueries
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                left.visit_queries_mut(visit);
                right.visit_queries_mut(visit);
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for query in subqueries {
            query.visit_queries_mut(visit);
        }
    }
}

fn visit_source(source: &mut SourcePlan, visit: &mut dyn FnMut(&mut QueryPlan)) {
    match source {
        SourcePlan::Subquery { body, .. } => body.visit_queries_mut(visit),
        SourcePlan::Table {
            executable_query: Some(body),
            ..
        } => body.visit_queries_mut(visit),
        SourcePlan::Join { left, right, .. } => {
            visit_source(left, visit);
            visit_source(right, visit);
        }
        _ => {}
    }
}
