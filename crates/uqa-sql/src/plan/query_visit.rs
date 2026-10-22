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
        self.visit_queries_while_mut(&mut |query| {
            visit(query);
            true
        });
    }

    /// Visit command-owned query roots without descending into their query children.
    pub fn visit_query_roots_mut(&mut self, visit: &mut dyn FnMut(&mut QueryPlan)) {
        self.visit_queries_while_mut(&mut |query| {
            visit(query);
            false
        });
    }

    fn visit_queries_while_mut(&mut self, visit: &mut dyn FnMut(&mut QueryPlan) -> bool) {
        match self {
            Self::Query(query) => query.visit_queries_while_mut(visit),
            Self::Command(command) => visit_command(command, visit),
        }
    }
}

fn visit_ctes(ctes: &mut [CtePlan], visit: &mut dyn FnMut(&mut QueryPlan) -> bool) {
    for cte in ctes {
        match &mut cte.body {
            CtePlanBody::Query(query) => query.visit_queries_while_mut(visit),
            CtePlanBody::Command(command) => visit_command(command, visit),
        }
    }
}

fn visit_command(command: &mut CommandPlan, visit: &mut dyn FnMut(&mut QueryPlan) -> bool) {
    match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            query.visit_queries_while_mut(visit);
            return;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            body.visit_queries_while_mut(visit);
            return;
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                for query in &mut expression.subqueries {
                    query.visit_queries_while_mut(visit);
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
        query.visit_queries_while_mut(visit);
    }
    if let Some(source) = command.source_input_mut() {
        visit_source(source, visit);
    }
}

impl QueryPlan {
    pub fn visit_queries_mut(&mut self, visit: &mut dyn FnMut(&mut Self)) {
        self.visit_queries_while_mut(&mut |query| {
            visit(query);
            true
        });
    }

    fn visit_queries_while_mut(&mut self, visit: &mut dyn FnMut(&mut Self) -> bool) {
        if !visit(self) {
            return;
        }
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
                left.visit_queries_while_mut(visit);
                right.visit_queries_while_mut(visit);
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for query in subqueries {
            query.visit_queries_while_mut(visit);
        }
    }
}

fn visit_source(source: &mut SourcePlan, visit: &mut dyn FnMut(&mut QueryPlan) -> bool) {
    match source {
        SourcePlan::Subquery { body, .. } => body.visit_queries_while_mut(visit),
        SourcePlan::Table {
            executable_query: Some(body),
            ..
        } => body.visit_queries_while_mut(visit),
        SourcePlan::Join { left, right, .. } => {
            visit_source(left, visit);
            visit_source(right, visit);
        }
        _ => {}
    }
}

impl CommandPlan {
    pub fn visit_query_roots_mut(&mut self, visit: &mut dyn FnMut(&mut QueryPlan)) {
        visit_command(self, &mut |query| {
            visit(query);
            false
        });
    }
}
