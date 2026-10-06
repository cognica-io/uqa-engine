//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL function volatility and expression rewrite safety over catalog metadata.
//!
//! Volatility is a semantic property, not merely an optimizer hint.  A
//! `VOLATILE` call may not be duplicated, elided, moved to a different join
//! level, or hidden behind a statement-local view cache.  Keep the decision in
//! one place so the view, CTE, predicate-pushdown, column-pruning, and `DPccp`
//! paths cannot drift apart.

use std::collections::BTreeSet;

use crate::ast::{FunctionBinding, FunctionVolatility};
use crate::plan::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan};
use crate::SQLError;
use crate::ScalarExpr;

/// Metadata needed to classify SQL expressions without invoking a routine or reading a row.
pub trait VolatilityCatalog {
    fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility>;
    fn routine_volatilities(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>>;
    fn view_query(&self, name: &str) -> Result<Option<QueryPlan>, SQLError>;
}

use super::builtin_function_dispatch_name;

/// Resolve the volatility of the implementation that can run for `name`.
///
/// Rust extension callbacks default to `VOLATILE`, while registrations with
/// explicit options use their declared volatility. SQL routine overloads are
/// combined conservatively: a name is only non-volatile when every overload
/// registered under it is non-volatile. This remains correct before runtime
/// argument coercion selects a particular overload.
pub fn function_volatility(
    catalog: &dyn VolatilityCatalog,
    name: &str,
    argument_count: usize,
) -> FunctionVolatility {
    function_volatility_with_binding(catalog, name, None, argument_count)
}

pub fn function_binding_is_volatile(
    catalog: &dyn VolatilityCatalog,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
) -> bool {
    function_volatility_with_binding(catalog, name, binding, argument_count)
        == FunctionVolatility::Volatile
}

pub fn function_volatility_with_binding(
    catalog: &dyn VolatilityCatalog,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
) -> FunctionVolatility {
    let builtin = builtin_function_volatility(name, binding, argument_count);
    if matches!(
        binding.and_then(|binding| binding.dispatch),
        Some(crate::ast::FunctionDispatch::NumericOperator(_))
    ) || builtin == FunctionVolatility::Volatile
    {
        return builtin;
    }
    let identity = name.to_ascii_lowercase();

    // Registrations made through the original APIs retain the conservative
    // VOLATILE default. Explicit options let pure callbacks participate in
    // the same optimizer rules as declared SQL routines.
    if let Some(volatility) = catalog.host_function_volatility(&identity) {
        return volatility;
    }

    if let Some(volatility) = sql_routine_volatility(catalog, &identity, binding) {
        return volatility;
    }
    builtin
}

/// Classify an already selected built-in without consulting host or SQL routine registrations. Callers that have not resolved the routine must use the catalog-aware entry point instead.
pub fn builtin_function_volatility(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
) -> FunctionVolatility {
    if matches!(
        binding.and_then(|binding| binding.dispatch),
        Some(crate::ast::FunctionDispatch::NumericOperator(_))
    ) {
        return FunctionVolatility::Immutable;
    }
    let lower = builtin_function_dispatch_name(&name.to_ascii_lowercase());
    if builtin_is_volatile(&lower) {
        return FunctionVolatility::Volatile;
    }

    // UQA retrieval/graph functions not listed above read the statement's
    // catalog snapshot.  Session/catalog introspection functions have the same
    // statement-stable contract.  All remaining built-ins are value-pure.
    if crate::registry::is_registered(&lower)
        || matches!(
            lower.as_str(),
            "current_schema"
                | "concat"
                | "concat_ws"
                | "format"
                | "pg_typeof"
                | "typeof"
                | "row_to_json"
                | "to_json"
                | "to_jsonb"
                | "json_build_object"
                | "jsonb_build_object"
                | "json_build_array"
                | "jsonb_build_array"
                | "to_char"
                | "to_date"
                | "to_number"
                | "now"
                | "current_date"
                | "current_time"
                | "current_timestamp"
                | "localtime"
                | "localtimestamp"
                | "statement_timestamp"
                | "transaction_timestamp"
                | "current_schemas"
                | "current_setting"
                | "pg_backend_pid"
                | "pg_my_temp_schema"
                | "pg_is_other_temp_schema"
                | "pg_trigger_depth"
                | "version"
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
                | "pg_sequence_parameters"
                | "pg_get_triggerdef"
                | "pg_get_ruledef"
                | "pg_get_viewdef"
                | "pg_get_indexdef"
                | "pg_get_constraintdef"
                | "pg_get_function_arguments"
                | "pg_get_function_identity_arguments"
                | "pg_get_function_result"
                | "pg_get_function_sqlbody"
                | "format_type"
                | "pg_describe_object"
                | "pg_has_role"
                | "pg_get_userbyid"
                | "has_database_privilege"
                | "has_table_privilege"
                | "has_column_privilege"
                | "has_schema_privilege"
                | "has_sequence_privilege"
                | "has_function_privilege"
                | "has_type_privilege"
                // The enum support functions that read a type's label list see labels added later in the transaction.
                | "enum_first"
                | "enum_last"
                | "enum_range"
        )
        || builtin_overload_is_stable(&lower, binding, argument_count)
    {
        FunctionVolatility::Stable
    } else {
        FunctionVolatility::Immutable
    }
}

fn builtin_overload_is_stable(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
) -> bool {
    (name == "age" && argument_count == 1)
        || (name == "to_timestamp" && argument_count == 2)
        || (matches!(name, "quote_literal" | "quote_nullable")
            && binding.is_none_or(|binding| binding.argument_types.as_slice() != ["text"]))
        || (matches!(name, "date_part" | "extract" | "date_trunc")
            && argument_count == 2
            && binding.is_none_or(|binding| {
                binding
                    .argument_types
                    .iter()
                    .any(|ty| matches!(ty.as_str(), "timestamptz" | "timestamp with time zone"))
            }))
}

// These built-ins mutate catalog/session state or derive a fresh value on every evaluation.
fn builtin_is_volatile(name: &str) -> bool {
    matches!(
        name,
        "random"
            | "setseed"
            | "set_config"
            | "pg_sleep"
            | "pg_sleep_for"
            | "pg_sleep_until"
            | "pg_notify"
            | "pg_notification_queue_usage"
            | "array_sample"
            | "nextval"
            | "currval"
            | "lastval"
            | "setval"
            | "pg_get_sequence_data"
            | "pg_sequence_last_value"
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
    )
}

fn sql_routine_volatility(
    catalog: &dyn VolatilityCatalog,
    identity: &str,
    binding: Option<&FunctionBinding>,
) -> Option<FunctionVolatility> {
    let overloads = catalog.routine_volatilities(identity, binding)?;
    if overloads.contains(&FunctionVolatility::Volatile) {
        return Some(FunctionVolatility::Volatile);
    }
    if overloads.contains(&FunctionVolatility::Stable) {
        return Some(FunctionVolatility::Stable);
    }
    Some(FunctionVolatility::Immutable)
}

pub fn expr_contains_volatile_function(catalog: &dyn VolatilityCatalog, expr: &ScalarExpr) -> bool {
    expr_contains_volatile_function_with(catalog, expr, true)
}

/// Query-level walks inspect a block's subquery plans themselves, so their expression scan treats a subquery reference as opaque-but-inspected (`conservative_subqueries == false`) instead of assuming volatility.
fn expr_contains_volatile_function_with(
    catalog: &dyn VolatilityCatalog,
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
                    function_volatility_with_binding(catalog, name, binding.as_ref(), args.len())
                        == FunctionVolatility::Volatile;
            }
            ScalarExpr::WindowCall { name, args, .. } => {
                volatile =
                    function_volatility(catalog, name, args.len()) == FunctionVolatility::Volatile;
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
pub fn select_contains_volatile_function(
    catalog: &dyn VolatilityCatalog,
    block: &QueryBlockPlan,
) -> bool {
    block
        .projections
        .iter()
        .any(|projection| expr_contains_volatile_function_with(catalog, &projection.expr, false))
        || block
            .r#where
            .as_ref()
            .is_some_and(|expr| expr_contains_volatile_function_with(catalog, expr, false))
        || block
            .group_by
            .iter()
            .any(|expr| expr_contains_volatile_function_with(catalog, expr, false))
        || block.grouping_sets.iter().any(|set| {
            set.iter()
                .any(|expr| expr_contains_volatile_function_with(catalog, expr, false))
        })
        || block
            .having
            .as_ref()
            .is_some_and(|expr| expr_contains_volatile_function_with(catalog, expr, false))
        || block
            .order_by
            .iter()
            .any(|order| expr_contains_volatile_function_with(catalog, &order.expr, false))
        || block
            .limit
            .as_ref()
            .is_some_and(|expr| expr_contains_volatile_function_with(catalog, expr, false))
        || block
            .offset
            .as_ref()
            .is_some_and(|expr| expr_contains_volatile_function_with(catalog, expr, false))
        || block
            .distinct_on
            .iter()
            .any(|expr| expr_contains_volatile_function_with(catalog, expr, false))
}

/// Inspect a complete query, including transitive view dependencies.
pub fn query_contains_volatile_function(
    catalog: &dyn VolatilityCatalog,
    plan: &QueryPlan,
) -> Result<bool, SQLError> {
    query_contains_volatile_function_inner(catalog, plan, &mut BTreeSet::new())
}

fn query_contains_volatile_function_inner(
    catalog: &dyn VolatilityCatalog,
    plan: &QueryPlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    for cte in &plan.ctes {
        if match &cte.body {
            crate::plan::CtePlanBody::Query(query) => {
                query_contains_volatile_function_inner(catalog, query, visiting_views)?
            }
            crate::plan::CtePlanBody::Command(_) => true,
        } {
            return Ok(true);
        }
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            if select_contains_volatile_function(catalog, block) {
                return Ok(true);
            }
            for subquery in &block.subqueries {
                if query_contains_volatile_function_inner(catalog, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
            if let Some(source) = &block.from {
                source_contains_volatile_function(catalog, source, visiting_views)
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
            if query_contains_volatile_function_inner(catalog, left, visiting_views)?
                || query_contains_volatile_function_inner(catalog, right, visiting_views)?
                || order_by
                    .iter()
                    .any(|order| expr_contains_volatile_function(catalog, &order.expr))
                || limit
                    .as_ref()
                    .is_some_and(|expr| expr_contains_volatile_function(catalog, expr))
                || offset
                    .as_ref()
                    .is_some_and(|expr| expr_contains_volatile_function(catalog, expr))
            {
                return Ok(true);
            }
            for subquery in subqueries {
                if query_contains_volatile_function_inner(catalog, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        RelationalPlan::Values { rows, subqueries } => {
            if rows
                .iter()
                .flatten()
                .any(|expr| expr_contains_volatile_function(catalog, expr))
            {
                return Ok(true);
            }
            for subquery in subqueries {
                if query_contains_volatile_function_inner(catalog, subquery, visiting_views)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

fn source_contains_volatile_function(
    catalog: &dyn VolatilityCatalog,
    source: &SourcePlan,
    visiting_views: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    match source {
        SourcePlan::Table { name, .. } => {
            let key = name.to_ascii_lowercase();
            if !visiting_views.insert(key.clone()) {
                return Ok(false);
            }
            let result = match catalog.view_query(name)? {
                Some(view) => {
                    query_contains_volatile_function_inner(catalog, &view, visiting_views)
                }
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
                .is_some_and(|expr| expr_contains_volatile_function(catalog, expr))
            {
                return Ok(true);
            }
            Ok(
                source_contains_volatile_function(catalog, left, visiting_views)?
                    || source_contains_volatile_function(catalog, right, visiting_views)?,
            )
        }
        SourcePlan::Values { rows, .. } => Ok(rows
            .iter()
            .flatten()
            .any(|expr| expr_contains_volatile_function(catalog, expr))),
        SourcePlan::Function {
            name,
            binding,
            args,
            ..
        } => Ok(
            function_volatility_with_binding(catalog, name, binding.as_ref(), args.len())
                == FunctionVolatility::Volatile
                || args
                    .iter()
                    .any(|expr| expr_contains_volatile_function(catalog, expr)),
        ),
        SourcePlan::FunctionGroup { functions, .. } => Ok(functions.iter().any(|function| {
            function_volatility_with_binding(
                catalog,
                &function.name,
                function.binding.as_ref(),
                function.args.len(),
            ) == FunctionVolatility::Volatile
                || function
                    .args
                    .iter()
                    .any(|expr| expr_contains_volatile_function(catalog, expr))
        })),
        SourcePlan::Subquery { body, .. } => {
            query_contains_volatile_function_inner(catalog, body, visiting_views)
        }
    }
}

/// Whether scalar optimizer rewrites or `DPccp` join enumeration must be kept
/// away from a plan.  `rewrite_scalar_expressions` is exhaustive over query,
/// mutation, CTE, prepared/explained, and expression-plan children.
pub fn unified_plan_contains_volatile_function(
    catalog: &dyn VolatilityCatalog,
    plan: &UnifiedPlan,
) -> bool {
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
                    function_volatility_with_binding(catalog, name, binding.as_ref(), args.len())
                        == FunctionVolatility::Volatile;
            }
            ScalarExpr::WindowCall { name, args, .. } => {
                volatile =
                    function_volatility(catalog, name, args.len()) == FunctionVolatility::Volatile;
            }
            _ => {}
        }
    });
    volatile
}

#[cfg(test)]
mod tests {
    use super::{
        expr_contains_volatile_function, FunctionBinding, FunctionVolatility, QueryPlan, SQLError,
        ScalarExpr, VolatilityCatalog,
    };

    struct EmptyCatalog;
    impl VolatilityCatalog for EmptyCatalog {
        fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
            None
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&FunctionBinding>,
        ) -> Option<Vec<FunctionVolatility>> {
            None
        }
        fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
            Ok(None)
        }
    }
    use crate::ast::{FrameExclusion, FrameMode};
    use crate::{ScalarFrameBound, ScalarWindowFrame, ScalarWindowSpec};

    #[test]
    fn builtin_constants_preserve_postgresql_statement_and_session_inputs() {
        // Checked against PostgreSQL 18.4 pg_proc.provolatile before enabling bound built-in constant folding.
        for name in [
            "concat",
            "concat_ws",
            "format",
            "pg_typeof",
            "row_to_json",
            "to_json",
            "to_jsonb",
            "json_build_object",
            "jsonb_build_array",
            "to_char",
            "to_date",
            "to_number",
            "has_table_privilege",
            "has_column_privilege",
        ] {
            assert_eq!(
                super::builtin_function_volatility(name, None, 2),
                FunctionVolatility::Stable,
                "{name}"
            );
        }
        for name in ["repeat", "upper"] {
            assert_eq!(
                super::builtin_function_volatility(name, None, 2),
                FunctionVolatility::Immutable,
                "{name}"
            );
        }
        assert_eq!(
            super::builtin_function_volatility("nextval", None, 1),
            FunctionVolatility::Volatile
        );
    }

    #[test]
    fn sequence_introspection_volatility_matches_postgresql() {
        for (name, expected) in [
            ("pg_get_sequence_data", FunctionVolatility::Volatile),
            ("pg_sequence_last_value", FunctionVolatility::Volatile),
            ("pg_sequence_parameters", FunctionVolatility::Stable),
        ] {
            for qualified in [name.to_string(), format!("pg_catalog.{name}")] {
                assert_eq!(
                    super::function_volatility(&EmptyCatalog, &qualified, 1),
                    expected,
                    "{qualified}"
                );
            }
        }
    }

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
                        order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
                        name: "random".into(),
                        binding: None,
                        args: Vec::new(),
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    })),
                    end: ScalarFrameBound::CurrentRow,
                    between: true,
                    exclusion: FrameExclusion::NoOthers,
                }),
            },
            filter: None,
            modifiers: crate::ast::WindowCallModifiers::default(),
        };
        assert!(expr_contains_volatile_function(&EmptyCatalog, &expression));
    }
}
