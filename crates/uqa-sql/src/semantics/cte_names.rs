//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enumerate the CTE names owned by a query and its sources.

use crate::plan::{QueryPlan, RelationalPlan, SourcePlan};
use std::collections::BTreeSet;

pub fn query_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_query_cte_names(plan, &mut names);
    names
}

pub fn collect_query_cte_names(plan: &QueryPlan, names: &mut BTreeSet<String>) {
    for cte in &plan.ctes {
        names.insert(cte.name.clone());
        collect_cte_body_names(&cte.body, names);
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_source_query_cte_names(source, names);
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            collect_query_cte_names(left, names);
            collect_query_cte_names(right, names);
        }
        RelationalPlan::Values { .. } => {}
    }
}

fn collect_cte_body_names(body: &crate::plan::CtePlanBody, names: &mut BTreeSet<String>) {
    match body {
        crate::plan::CtePlanBody::Query(query) => collect_query_cte_names(query, names),
        crate::plan::CtePlanBody::Command(command) => {
            for cte in command.ctes() {
                names.insert(cte.name.clone());
                collect_cte_body_names(&cte.body, names);
            }
            for query in command.query_inputs() {
                collect_query_cte_names(query, names);
            }
            if let Some(source) = command.source_input() {
                collect_source_query_cte_names(source, names);
            }
        }
    }
}

pub fn collect_source_query_cte_names(source: &SourcePlan, names: &mut BTreeSet<String>) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            collect_source_query_cte_names(left, names);
            collect_source_query_cte_names(right, names);
        }
        SourcePlan::Subquery { body, .. } => collect_query_cte_names(body, names),
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}
