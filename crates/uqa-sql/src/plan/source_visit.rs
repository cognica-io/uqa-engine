//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed source traversal includes nested queries, CTEs and command inputs.

use super::{
    CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};

impl super::PlanRef<'_> {
    pub fn visit_sources(&self, visit: &mut dyn FnMut(&SourcePlan)) {
        match self {
            Self::Query(query) => query.visit_sources(visit),
            Self::Command(command) => visit_command(command, visit),
        }
    }
}

impl UnifiedPlan {
    pub fn visit_sources(&self, visit: &mut dyn FnMut(&SourcePlan)) {
        match self {
            Self::Query(query) => query.visit_sources(visit),
            Self::Command(command) => visit_command(command, visit),
        }
    }
}

fn visit_ctes(ctes: &[CtePlan], visit: &mut dyn FnMut(&SourcePlan)) {
    for cte in ctes {
        match &cte.body {
            CtePlanBody::Query(query) => query.visit_sources(visit),
            CtePlanBody::Command(command) => visit_command(command, visit),
        }
    }
}

fn visit_command(command: &CommandPlan, visit: &mut dyn FnMut(&SourcePlan)) {
    match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            query.visit_sources(visit);
            return;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            body.visit_sources(visit);
            return;
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for input in params {
                for query in &input.subqueries {
                    query.visit_sources(visit);
                }
            }
            return;
        }
        _ => {}
    }
    visit_ctes(command.ctes(), visit);
    for query in command.query_inputs() {
        query.visit_sources(visit);
    }
    if let Some(source) = command.source_input() {
        visit_source(source, visit);
    }
}

impl QueryPlan {
    pub fn visit_sources(&self, visit: &mut dyn FnMut(&SourcePlan)) {
        visit_ctes(&self.ctes, visit);
        let subqueries = match &self.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &block.from {
                    visit_source(source, visit);
                }
                &block.subqueries
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                left.visit_sources(visit);
                right.visit_sources(visit);
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for query in subqueries {
            query.visit_sources(visit);
        }
    }
}

fn visit_source(source: &SourcePlan, visit: &mut dyn FnMut(&SourcePlan)) {
    visit(source);
    match source {
        SourcePlan::Subquery { body, .. } => body.visit_sources(visit),
        SourcePlan::Join { left, right, .. } => {
            visit_source(left, visit);
            visit_source(right, visit);
        }
        _ => {}
    }
}

impl UnifiedPlan {
    pub fn visit_sources_mut(&mut self, visit: &mut dyn FnMut(&mut SourcePlan)) {
        match self {
            Self::Query(query) => query.visit_sources_mut(visit),
            Self::Command(command) => visit_command_mut(command, visit),
        }
    }
}

pub(super) fn visit_command_mut(command: &mut CommandPlan, visit: &mut dyn FnMut(&mut SourcePlan)) {
    match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            query.visit_sources_mut(visit);
            return;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            body.visit_sources_mut(visit);
            return;
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for input in params {
                for query in &mut input.subqueries {
                    query.visit_sources_mut(visit);
                }
            }
            return;
        }
        _ => {}
    }
    if let Some(ctes) = command.ctes_mut() {
        for cte in ctes {
            match &mut cte.body {
                CtePlanBody::Query(query) => query.visit_sources_mut(visit),
                CtePlanBody::Command(command) => visit_command_mut(command, visit),
            }
        }
    }
    for query in command.query_inputs_mut() {
        query.visit_sources_mut(visit);
    }
    if let Some(source) = command.source_input_mut() {
        super::rewrite::visit_source(source, visit);
    }
}
