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

    /// Lend the selected view plan for one read. Catalogs retaining immutable definitions can override this adapter without constructing an owned plan.
    fn with_view_query(
        &self,
        name: &str,
        read: &mut dyn FnMut(&QueryPlan) -> Result<bool, SQLError>,
    ) -> Result<Option<bool>, SQLError> {
        self.view_query(name)?.as_ref().map(read).transpose()
    }
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

fn is_intrinsic_scalar_syntax(binding: Option<&FunctionBinding>) -> bool {
    binding
        .and_then(|binding| binding.dispatch)
        .is_some_and(crate::ast::FunctionDispatch::is_concatenation)
        || matches!(
            binding.and_then(|binding| binding.dispatch),
            Some(
                crate::ast::FunctionDispatch::NumericOperator(_)
                    | crate::ast::FunctionDispatch::Pattern { .. }
                    | crate::ast::FunctionDispatch::PatternEscape { .. }
                    | crate::ast::FunctionDispatch::Collate { .. }
                    | crate::ast::FunctionDispatch::BooleanTest(_)
                    | crate::ast::FunctionDispatch::ArraySubscripts
                    | crate::ast::FunctionDispatch::ArraySlices
                    | crate::ast::FunctionDispatch::ArraySlicesWithBounds(_)
            )
        )
}

pub fn function_volatility_with_binding(
    catalog: &dyn VolatilityCatalog,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_count: usize,
) -> FunctionVolatility {
    let builtin = builtin_function_volatility(name, binding, argument_count);
    if is_intrinsic_scalar_syntax(binding) || builtin == FunctionVolatility::Volatile {
        return builtin;
    }
    let identity = super::functions::lowercase_function_name(name);

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
    if let Some(crate::ast::FunctionDispatch::Concat { oid }) =
        binding.and_then(|binding| binding.dispatch)
    {
        return oid
            .and_then(|oid| crate::type_resolution::binary_operator_by_oid(i64::from(oid)))
            .filter(|operator| operator.name == "||")
            .map_or(FunctionVolatility::Stable, |operator| operator.volatility());
    }
    if is_intrinsic_scalar_syntax(binding) {
        return FunctionVolatility::Immutable;
    }
    let lower = builtin_function_dispatch_name(name);
    if builtin_is_volatile(&lower) {
        return FunctionVolatility::Volatile;
    }

    // UQA retrieval/graph functions not listed above read the statement's
    // catalog snapshot.  Session/catalog introspection functions have the same
    // statement-stable contract.  All remaining built-ins are value-pure.
    if crate::registry::is_registered(&lower)
        || matches!(
            lower.as_ref(),
            "current_schema"
                | "concat"
                | "concat_ws"
                | "format"
                | "pg_typeof"
                | "pg_collation_for"
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
                | "to_regcollation"
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
                | "pg_get_functiondef"
            | "pg_get_function_sqlbody"
                | "format_type"
                | "pg_describe_object"
            | "obj_description"
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
    (matches!(name, "age" | "timezone") && argument_count == 1)
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

/// Inspect calls in this scalar expression only. The caller must inspect query-valued children through their owning arena before moving or duplicating an expression.
pub fn scalar_calls_are_volatile(catalog: &dyn VolatilityCatalog, expr: &ScalarExpr) -> bool {
    expr_contains_volatile_function_with(catalog, expr, false)
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

/// Inspect a scalar plan with its query arena instead of treating every subquery as an opaque volatile call.
pub fn expression_plan_contains_volatile_function(
    catalog: &dyn VolatilityCatalog,
    plan: &crate::plan::ExpressionPlan,
) -> Result<bool, SQLError> {
    scalar_with_subqueries_contains_volatile_function(catalog, &plan.scalar, &plan.subqueries)
}

/// Inspect a borrowed expression and only the query children it references in its owning arena. Other projections may share that arena without contributing effects to this expression.
pub fn scalar_with_subqueries_contains_volatile_function(
    catalog: &dyn VolatilityCatalog,
    scalar: &ScalarExpr,
    subqueries: &[QueryPlan],
) -> Result<bool, SQLError> {
    scalar_in_scope_contains_volatile_function(catalog, scalar, subqueries, &|_| false)
}

/// Inspect an expression's referenced subqueries under its enclosing CTE namespace. Those external producers are owned and evaluated by their defining query, not by this scalar expression.
pub fn scalar_in_scope_contains_volatile_function(
    catalog: &dyn VolatilityCatalog,
    scalar: &ScalarExpr,
    subqueries: &[QueryPlan],
    visible_ctes: &dyn Fn(&str) -> bool,
) -> Result<bool, SQLError> {
    if scalar_calls_are_volatile(catalog, scalar) {
        return Ok(true);
    }
    let mut result = Ok(false);
    scalar.visit(&mut |node| {
        if !matches!(result, Ok(false)) {
            return;
        }
        let index = match node {
            ScalarExpr::ScalarSubquery(index)
            | ScalarExpr::Exists {
                subquery: index, ..
            }
            | ScalarExpr::InSubquery {
                subquery: index, ..
            } => *index,
            _ => return,
        };
        // An unresolved arena reference cannot authorize eliminating its expression; the ordinary validation path retains its diagnostic.
        result = subqueries.get(index).map_or(Ok(true), |query| {
            query_contains_volatile_function_inner(
                catalog,
                query,
                &mut BTreeSet::new(),
                visible_ctes,
            )
        });
    });
    result
}

/// The block's own subquery plans are inspected separately by the query-level walk, so subquery references here are not conservatively volatile.
pub fn select_contains_volatile_function(
    catalog: &dyn VolatilityCatalog,
    block: &QueryBlockPlan,
) -> bool {
    block
        .deferred_outer_rewrites
        .iter()
        .flat_map(super::view_rewrite::DeferredOuterRewrite::expressions)
        .any(|expression| expr_contains_volatile_function_with(catalog, &expression.scalar, true))
        || block.projections.iter().any(|projection| {
            expr_contains_volatile_function_with(catalog, &projection.expr, false)
        })
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
    query_contains_volatile_function_inner(catalog, plan, &mut BTreeSet::new(), &|_| false)
}

fn query_contains_volatile_function_inner(
    catalog: &dyn VolatilityCatalog,
    plan: &QueryPlan,
    visiting_views: &mut BTreeSet<String>,
    outer: &dyn Fn(&str) -> bool,
) -> Result<bool, SQLError> {
    let scope = |name: &str| plan.ctes.iter().any(|cte| cte.name == name) || outer(name);
    let recursive = plan.ctes.iter().any(|cte| cte.recursive);
    for (index, cte) in plan.ctes.iter().enumerate() {
        let visible = if recursive {
            &plan.ctes[..]
        } else {
            &plan.ctes[..index]
        };
        let definition_scope =
            |name: &str| visible.iter().any(|cte| cte.name == name) || outer(name);
        if match &cte.body {
            crate::plan::CtePlanBody::Query(query) => query_contains_volatile_function_inner(
                catalog,
                query,
                visiting_views,
                &definition_scope,
            )?,
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
                if query_contains_volatile_function_inner(
                    catalog,
                    subquery,
                    visiting_views,
                    &scope,
                )? {
                    return Ok(true);
                }
            }
            if let Some(source) = &block.from {
                source_contains_volatile_function(catalog, source, visiting_views, &scope)
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
            if query_contains_volatile_function_inner(catalog, left, visiting_views, &scope)?
                || query_contains_volatile_function_inner(catalog, right, visiting_views, &scope)?
                || order_by
                    .iter()
                    .any(|order| scalar_calls_are_volatile(catalog, &order.expr))
                || limit
                    .as_ref()
                    .is_some_and(|expr| scalar_calls_are_volatile(catalog, expr))
                || offset
                    .as_ref()
                    .is_some_and(|expr| scalar_calls_are_volatile(catalog, expr))
            {
                return Ok(true);
            }
            for subquery in subqueries {
                if query_contains_volatile_function_inner(
                    catalog,
                    subquery,
                    visiting_views,
                    &scope,
                )? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        RelationalPlan::Values { rows, subqueries } => {
            if rows
                .iter()
                .flatten()
                .any(|expr| scalar_calls_are_volatile(catalog, expr))
            {
                return Ok(true);
            }
            for subquery in subqueries {
                if query_contains_volatile_function_inner(
                    catalog,
                    subquery,
                    visiting_views,
                    &scope,
                )? {
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
    scope: &dyn Fn(&str) -> bool,
) -> Result<bool, SQLError> {
    match source {
        crate::plan::SourcePlan::RetainedRows(_) => Ok(false),
        SourcePlan::Table { name, .. } => {
            if super::cte_reference_name(name).is_some_and(|name| scope(&name)) {
                // Each visible CTE definition was already inspected in its defining query. A same-named catalog view is a different relation.
                return Ok(false);
            }
            let key = name.to_ascii_lowercase();
            if !visiting_views.insert(key.clone()) {
                return Ok(false);
            }
            let result = catalog
                .with_view_query(name, &mut |view| {
                    query_contains_volatile_function_inner(catalog, view, visiting_views, &|_| {
                        false
                    })
                })
                .map(|result| result.unwrap_or(false));
            visiting_views.remove(&key);
            result
        }
        SourcePlan::Join {
            left, right, on, ..
        } => {
            if on
                .as_ref()
                .is_some_and(|expr| scalar_calls_are_volatile(catalog, expr))
            {
                return Ok(true);
            }
            Ok(
                source_contains_volatile_function(catalog, left, visiting_views, scope)?
                    || source_contains_volatile_function(catalog, right, visiting_views, scope)?,
            )
        }
        SourcePlan::Values { rows, .. } => Ok(rows
            .iter()
            .flatten()
            .any(|expr| scalar_calls_are_volatile(catalog, expr))),
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
                    .any(|expr| scalar_calls_are_volatile(catalog, expr)),
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
                    .any(|expr| scalar_calls_are_volatile(catalog, expr))
        })),
        SourcePlan::Subquery { body, .. } => {
            query_contains_volatile_function_inner(catalog, body, visiting_views, scope)
        }
    }
}

/// Whether scalar optimizer rewrites or `DPccp` join enumeration must preserve volatile call scheduling. Borrow the plan and its expression roots; inspecting constants must not copy their retained payloads or allocate command child lists.
pub fn unified_plan_contains_volatile_function(
    catalog: &dyn VolatilityCatalog,
    plan: &UnifiedPlan,
) -> bool {
    let mut volatile = false;
    plan.visit_scalar_expressions(&mut |expression| {
        if !volatile {
            volatile = scalar_calls_are_volatile(catalog, expression);
        }
    });
    volatile
}

#[cfg(test)]
mod tests;
