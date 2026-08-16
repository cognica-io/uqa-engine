//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{builtin_function_dispatch_name, BTreeSet, Engine, SQLError};

/// SELECT is not synonymous with read-only: UQA exposes a small set of
/// state-changing scalar functions, and SQL/PLpgSQL routines invoked from a
/// projection can contain commands. Classify those plans before choosing the
/// transaction mode so memory execution takes a rollback snapshot and `SQLite`
/// opens a write transaction. Cloning the plan is bounded by query size and
/// avoids the database-sized deep copy paid by a full memory snapshot.
pub(super) fn query_may_mutate_engine(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
) -> Result<bool, SQLError> {
    query_may_mutate_engine_inner(engine, query, &mut BTreeSet::new())
}

fn query_may_mutate_engine_inner(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    if query_source_may_mutate_engine(engine, query, visiting_views)? {
        return Ok(true);
    }
    let mut plan = uqa_planner::UnifiedPlan::Query(Box::new(query.clone()));
    let mut mutates = uqa_planner::optimizer::query_contains_implicit_hybrid_fusion(query);
    plan.rewrite_scalar_expressions(&mut |expression| {
        let uqa_execution::ScalarExpr::Func { name, .. } = expression else {
            return;
        };
        mutates |= function_may_mutate_engine(engine, name);
    });
    Ok(mutates)
}

fn function_may_mutate_engine(engine: &Engine, name: &str) -> bool {
    let identity = name.to_ascii_lowercase();
    let dispatch_name = builtin_function_dispatch_name(&identity);
    matches!(
        dispatch_name.as_str(),
        "create_analyzer"
            | "drop_analyzer"
            | "set_table_analyzer"
            | "graph_create"
            | "graph_drop"
            | "create_graph"
            | "drop_graph"
            | "create_vlabel"
            | "create_elabel"
            | "drop_label"
            | "alter_graph"
            | "cypher"
            | "deep_learn"
            | "bayesian_match"
            | "bayesian_match_with_prior"
            | "fts_match"
            | "multi_field_match"
            | "nextval"
            | "random"
            | "setval"
            | "setseed"
    ) || engine.registered_runtime_function_may_mutate_engine(&identity)
        || engine.lookup_sql_functions(&identity).is_some()
}

fn query_source_may_mutate_engine(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    for cte in &query.ctes {
        if query_source_may_mutate_engine(engine, &cte.query, visiting_views)? {
            return Ok(true);
        }
    }
    match &query.root {
        uqa_planner::RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_ref() {
                if source_may_mutate_engine(engine, source, visiting_views)? {
                    return Ok(true);
                }
            }
            for subquery in &block.subqueries {
                if query_source_may_mutate_engine(engine, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
        }
        uqa_planner::RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            if query_source_may_mutate_engine(engine, left, visiting_views)?
                || query_source_may_mutate_engine(engine, right, visiting_views)?
            {
                return Ok(true);
            }
            for subquery in subqueries {
                if query_source_may_mutate_engine(engine, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
        }
        uqa_planner::RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                if query_source_may_mutate_engine(engine, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn source_may_mutate_engine(
    engine: &Engine,
    source: &uqa_planner::SourcePlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    match source {
        uqa_planner::SourcePlan::Function { name, .. } => {
            Ok(function_may_mutate_engine(engine, name))
        }
        uqa_planner::SourcePlan::Join { left, right, .. } => {
            Ok(source_may_mutate_engine(engine, left, visiting_views)?
                || source_may_mutate_engine(engine, right, visiting_views)?)
        }
        uqa_planner::SourcePlan::Subquery { body, .. } => {
            query_source_may_mutate_engine(engine, body, visiting_views)
        }
        uqa_planner::SourcePlan::Table { name, .. } => {
            let key = name.to_ascii_lowercase();
            if !visiting_views.insert(key.clone()) {
                return Ok(false);
            }
            let result = match engine.view_plan(name) {
                Ok(Some(view)) => query_may_mutate_engine_inner(engine, &view, visiting_views),
                Ok(None) => Ok(false),
                Err(error) => Err(error),
            };
            visiting_views.remove(&key);
            result
        }
        uqa_planner::SourcePlan::Values { .. } => Ok(false),
    }
}

pub(super) fn is_transaction_control(plan: &uqa_planner::UnifiedPlan) -> bool {
    matches!(
        plan,
        uqa_planner::UnifiedPlan::Command(command)
            if matches!(command.as_ref(), uqa_planner::CommandPlan::Transaction(_))
    )
}
