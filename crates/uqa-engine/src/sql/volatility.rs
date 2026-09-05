//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-aware SQL function volatility.
//!
//! Volatility is a semantic property, not merely an optimizer hint.  A
//! `VOLATILE` call may not be duplicated, elided, moved to a different join
//! level, or hidden behind a statement-local view cache.  Keep the decision in
//! one place so the view, CTE, predicate-pushdown, column-pruning, and `DPccp`
//! paths cannot drift apart.

use std::collections::BTreeSet;

use uqa_execution::ScalarExpr;
use uqa_planner::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan};
use uqa_sql::ast::{FunctionBinding, FunctionVolatility};
use uqa_sql::SQLError;

use crate::Engine;

use super::builtin_function_dispatch_name;

/// Resolve the volatility of the implementation that can run for `name`.
///
/// Rust extension callbacks default to `VOLATILE`, while registrations with
/// explicit options use their declared volatility. SQL routine overloads are
/// combined conservatively: a name is only non-volatile when every overload
/// registered under it is non-volatile. This remains correct before runtime
/// argument coercion selects a particular overload.
pub(super) fn function_volatility(
    engine: &Engine,
    name: &str,
    argument_count: usize,
) -> FunctionVolatility {
    function_volatility_with_binding(engine, name, None, argument_count)
}

pub(super) fn function_binding_is_volatile(
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
) -> bool {
    function_volatility_with_binding(engine, name, binding, argument_count)
        == FunctionVolatility::Volatile
}

pub(super) fn function_volatility_with_binding(
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
) -> FunctionVolatility {
    let identity = name.to_ascii_lowercase();
    let lower = builtin_function_dispatch_name(&identity);

    // These implementations either mutate engine/session state or derive a
    // fresh value on every evaluation.  `now`/`current_timestamp`,
    // `statement_timestamp`, and `current_date` are intentionally included:
    // the scalar evaluator currently obtains wall-clock time per call rather
    // than owning a statement timestamp snapshot.
    if matches!(
        lower.as_str(),
        "random"
            | "setseed"
            | "pg_notify"
            | "pg_notification_queue_usage"
            | "array_sample"
            | "nextval"
            | "currval"
            | "lastval"
            | "setval"
            | "now"
            | "current_date"
            | "current_time"
            | "current_timestamp"
            | "localtime"
            | "localtimestamp"
            | "statement_timestamp"
            | "transaction_timestamp"
            | "clock_timestamp"
            | "timeofday"
            | "gen_random_uuid"
            | "uuidv4"
            | "uuidv7"
            | "create_analyzer"
            | "drop_analyzer"
            | "set_table_analyzer"
            | "graph_create"
            | "graph_drop"
            | "create_graph"
            | "drop_graph"
            | "graph_exists"
            | "create_vlabel"
            | "create_elabel"
            | "drop_label"
            | "alter_graph"
            | "cypher"
            | "deep_learn"
            // Retrieval calibration learns and persists parameters on a
            // cache miss; it therefore is not a read-only scalar operation.
            | "bayesian_match"
            | "bayesian_match_with_prior"
            | "fts_match"
            | "multi_field_match"
    ) || (lower == "age" && argument_count == 1)
    {
        return FunctionVolatility::Volatile;
    }

    // Registrations made through the original APIs retain the conservative
    // VOLATILE default. Explicit options let pure callbacks participate in
    // the same optimizer rules as declared SQL routines.
    if let Some(volatility) = engine.registered_runtime_function_volatility(&identity) {
        return volatility;
    }

    if let Some(volatility) = sql_routine_volatility(engine, &identity, binding) {
        return volatility;
    }

    // UQA retrieval/graph functions not listed above read the statement's
    // engine snapshot.  Session/catalog introspection functions have the same
    // statement-stable contract.  All remaining built-ins are value-pure.
    if uqa_sql::registry::is_registered(&lower)
        || matches!(
            lower.as_str(),
            "current_schema"
                | "current_schemas"
                | "pg_backend_pid"
                | "pg_listening_channels"
                | "to_regclass"
                | "to_regnamespace"
                | "to_regproc"
                | "to_regprocedure"
                | "to_regrole"
                | "to_regtype"
                | "current_database"
                | "current_catalog"
                | "current_user"
                | "session_user"
                | "list_analyzers"
                | "fts_index_stats"
                | "pg_get_expr"
                | "pg_get_partkeydef"
                | "pg_get_serial_sequence"
                | "pg_get_triggerdef"
                | "pg_get_ruledef"
                | "pg_get_viewdef"
                | "pg_get_indexdef"
                | "pg_has_role"
                | "has_database_privilege"
                | "has_schema_privilege"
                | "has_sequence_privilege"
        )
    {
        FunctionVolatility::Stable
    } else {
        FunctionVolatility::Immutable
    }
}

fn sql_routine_volatility(
    engine: &Engine,
    identity: &str,
    binding: Option<&FunctionBinding>,
) -> Option<FunctionVolatility> {
    let overloads = match binding {
        Some(binding) if binding.builtin => None,
        Some(binding) => engine.lookup_bound_sql_functions_by_binding(binding),
        None => engine
            .lookup_visible_sql_functions_for_analysis(identity)
            .ok()
            .flatten(),
    }?;
    if overloads
        .iter()
        .any(|function| function.def.volatility == FunctionVolatility::Volatile)
    {
        return Some(FunctionVolatility::Volatile);
    }
    if overloads
        .iter()
        .any(|function| function.def.volatility == FunctionVolatility::Stable)
    {
        return Some(FunctionVolatility::Stable);
    }
    Some(FunctionVolatility::Immutable)
}

pub(super) fn expr_contains_volatile_function(engine: &Engine, expr: &ScalarExpr) -> bool {
    expr_contains_volatile_function_with(engine, expr, true)
}

/// Query-level walks inspect a block's subquery plans themselves, so their expression scan treats a subquery reference as opaque-but-inspected (`conservative_subqueries == false`) instead of assuming volatility.
fn expr_contains_volatile_function_with(
    engine: &Engine,
    expr: &ScalarExpr,
    conservative_subqueries: bool,
) -> bool {
    let mut volatile = false;
    expr.visit(&mut |part| {
        if volatile {
            return;
        }
        match part {
            ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } => {
                volatile =
                    function_volatility_with_binding(engine, name, binding.as_ref(), args.len())
                        == FunctionVolatility::Volatile;
            }
            ScalarExpr::WindowCall { name, args, .. } => {
                volatile =
                    function_volatility(engine, name, args.len()) == FunctionVolatility::Volatile;
            }
            // Query-valued children are inspected by the enclosing QueryPlan. At expression-only rewrite sites, retaining the conservative rule prevents an opaque child query from being duplicated or reordered.
            ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. }
            | ScalarExpr::InSubquery { .. } => volatile = conservative_subqueries,
            _ => {}
        }
    });
    volatile
}

/// The block's own subquery plans are inspected separately by the query-level walk, so subquery references here are not conservatively volatile.
pub(super) fn select_contains_volatile_function(engine: &Engine, block: &QueryBlockPlan) -> bool {
    block
        .projections
        .iter()
        .any(|projection| expr_contains_volatile_function_with(engine, &projection.expr, false))
        || block
            .r#where
            .as_ref()
            .is_some_and(|expr| expr_contains_volatile_function_with(engine, expr, false))
        || block
            .group_by
            .iter()
            .any(|expr| expr_contains_volatile_function_with(engine, expr, false))
        || block.grouping_sets.iter().any(|set| {
            set.iter()
                .any(|expr| expr_contains_volatile_function_with(engine, expr, false))
        })
        || block
            .having
            .as_ref()
            .is_some_and(|expr| expr_contains_volatile_function_with(engine, expr, false))
        || block
            .order_by
            .iter()
            .any(|order| expr_contains_volatile_function_with(engine, &order.expr, false))
        || block
            .limit
            .as_ref()
            .is_some_and(|expr| expr_contains_volatile_function_with(engine, expr, false))
        || block
            .offset
            .as_ref()
            .is_some_and(|expr| expr_contains_volatile_function_with(engine, expr, false))
        || block
            .distinct_on
            .iter()
            .any(|expr| expr_contains_volatile_function_with(engine, expr, false))
}

/// Inspect a complete query, including transitive view dependencies.
pub(super) fn query_contains_volatile_function(
    engine: &Engine,
    plan: &QueryPlan,
) -> Result<bool, SQLError> {
    query_contains_volatile_function_inner(engine, plan, &mut BTreeSet::new())
}

fn query_contains_volatile_function_inner(
    engine: &Engine,
    plan: &QueryPlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    for cte in &plan.ctes {
        if query_contains_volatile_function_inner(engine, &cte.query, visiting_views)? {
            return Ok(true);
        }
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            if select_contains_volatile_function(engine, block) {
                return Ok(true);
            }
            for subquery in &block.subqueries {
                if query_contains_volatile_function_inner(engine, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
            if let Some(source) = &block.from {
                source_contains_volatile_function(engine, source, visiting_views)
            } else {
                Ok(false)
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            if query_contains_volatile_function_inner(engine, left, visiting_views)?
                || query_contains_volatile_function_inner(engine, right, visiting_views)?
                || order_by
                    .iter()
                    .any(|order| expr_contains_volatile_function(engine, &order.expr))
                || limit
                    .as_ref()
                    .is_some_and(|expr| expr_contains_volatile_function(engine, expr))
                || offset
                    .as_ref()
                    .is_some_and(|expr| expr_contains_volatile_function(engine, expr))
            {
                return Ok(true);
            }
            for subquery in subqueries {
                if query_contains_volatile_function_inner(engine, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        RelationalPlan::Values { rows, subqueries } => {
            if rows
                .iter()
                .flatten()
                .any(|expr| expr_contains_volatile_function(engine, expr))
            {
                return Ok(true);
            }
            for subquery in subqueries {
                if query_contains_volatile_function_inner(engine, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

fn source_contains_volatile_function(
    engine: &Engine,
    source: &SourcePlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    match source {
        SourcePlan::Table { name, .. } => {
            let key = name.to_ascii_lowercase();
            if !visiting_views.insert(key.clone()) {
                return Ok(false);
            }
            let result = match engine.view_plan(name)? {
                Some(view) => query_contains_volatile_function_inner(engine, &view, visiting_views),
                None => Ok(false),
            };
            visiting_views.remove(&key);
            result
        }
        SourcePlan::Join {
            left, right, on, ..
        } => {
            if on
                .as_ref()
                .is_some_and(|expr| expr_contains_volatile_function(engine, expr))
            {
                return Ok(true);
            }
            Ok(
                source_contains_volatile_function(engine, left, visiting_views)?
                    || source_contains_volatile_function(engine, right, visiting_views)?,
            )
        }
        SourcePlan::Values { rows, .. } => Ok(rows
            .iter()
            .flatten()
            .any(|expr| expr_contains_volatile_function(engine, expr))),
        SourcePlan::Function {
            name,
            binding,
            args,
            ..
        } => Ok(
            function_volatility_with_binding(engine, name, binding.as_ref(), args.len())
                == FunctionVolatility::Volatile
                || args
                    .iter()
                    .any(|expr| expr_contains_volatile_function(engine, expr)),
        ),
        SourcePlan::FunctionGroup { functions, .. } => Ok(functions.iter().any(|function| {
            function_volatility_with_binding(
                engine,
                &function.name,
                function.binding.as_ref(),
                function.args.len(),
            ) == FunctionVolatility::Volatile
                || function
                    .args
                    .iter()
                    .any(|expr| expr_contains_volatile_function(engine, expr))
        })),
        SourcePlan::Subquery { body, .. } => {
            query_contains_volatile_function_inner(engine, body, visiting_views)
        }
    }
}

/// Whether scalar optimizer rewrites or `DPccp` join enumeration must be kept
/// away from a plan.  `rewrite_scalar_expressions` is exhaustive over query,
/// mutation, CTE, prepared/explained, and expression-plan children.
pub(super) fn unified_plan_contains_volatile_function(engine: &Engine, plan: &UnifiedPlan) -> bool {
    let mut inspected = plan.clone();
    let mut volatile = false;
    inspected.rewrite_scalar_expressions(&mut |expr| {
        if volatile {
            return;
        }
        match expr {
            ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } => {
                volatile =
                    function_volatility_with_binding(engine, name, binding.as_ref(), args.len())
                        == FunctionVolatility::Volatile;
            }
            ScalarExpr::WindowCall { name, args, .. } => {
                volatile =
                    function_volatility(engine, name, args.len()) == FunctionVolatility::Volatile;
            }
            _ => {}
        }
    });
    volatile
}

#[cfg(test)]
mod tests {
    use super::{expr_contains_volatile_function, Engine, ScalarExpr};
    use uqa_execution::{ScalarFrameBound, ScalarWindowFrame, ScalarWindowSpec};
    use uqa_sql::ast::FrameMode;

    #[test]
    fn volatility_inspection_includes_window_frame_expressions() {
        let expression = ScalarExpr::WindowCall {
            name: "sum".into(),
            args: vec![ScalarExpr::Column("amount".into())],
            spec: ScalarWindowSpec {
                partition_by: Vec::new(),
                order_by: Vec::new(),
                frame: Some(ScalarWindowFrame {
                    mode: FrameMode::Rows,
                    start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::Func {
                        name: "random".into(),
                        binding: None,
                        args: Vec::new(),
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    })),
                    end: ScalarFrameBound::CurrentRow,
                }),
            },
        };
        assert!(expr_contains_volatile_function(&Engine::new(), &expression));
    }
}
