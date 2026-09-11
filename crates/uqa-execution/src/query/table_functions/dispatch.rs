//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dispatch table-function calls after the streaming fast paths.

use super::{
    context::TableFunctionContext,
    records::{operator_join_rows, singleton_record_rows},
    registered_table_function_rows, TableFunctionCall, TableFunctionRows,
};
use crate::routines::invocation;
use crate::scalar::plan::PlanSubqueryArena;
use crate::{eval_call_arguments, ScalarEvalContext};
use uqa_core::Value;
use uqa_sql::{expr::unknown_function_error, SQLError};

#[allow(clippy::similar_names)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub(super) fn build_table_function_rows_with_row(
    context: &TableFunctionContext<'_>,
    call: TableFunctionCall<'_>,
    row: Option<&crate::OwnedPhysicalRow>,
) -> Result<TableFunctionRows, SQLError> {
    let TableFunctionCall {
        name,
        binding,
        relations,
        args,
        alias,
        column_aliases,
        column_types,
        ..
    } = call;
    let subquery_arena = PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
    let ctx = match row {
        Some(row) => ScalarEvalContext::from_row_lookup(row, context.params)
            .with_physical_outer_row(&row.schema, &row.row),
        None => ScalarEvalContext::new(None, context.params),
    };
    let ctx = ctx
        .with_function_hook(context.eval_hook)
        .with_subquery_runner(&subquery_arena);
    let identity = name.to_ascii_lowercase();
    let lower = uqa_sql::semantics::builtin_function_dispatch_name(&identity);
    if binding.is_none_or(|binding| binding.builtin)
        && uqa_sql::registry::is_operator_join_table_function(&lower)
    {
        let (relations, tree) = context
            .joins
            .lower(&lower, relations, args, context.params)?;
        let tuples = crate::operator_tree::joins::execute_cross_relation_operator_join(
            &context.retrieval,
            &relations,
            context.params,
            &tree,
        )?;
        return operator_join_rows(tuples, alias, column_aliases);
    }
    let call_args = eval_call_arguments(args, &ctx)?;
    let has_named_args = call_args.iter().any(|(name, _)| name.is_some());
    let evaluated: Vec<Value> = call_args.iter().map(|(_, value)| value.clone()).collect();
    if !has_named_args && binding.is_none_or(|binding| binding.builtin) {
        if let Some(result) = context
            .runtime
            .lookup_table_function(&identity)
            .map(|registration| registration.function.call(&evaluated))
        {
            return registered_table_function_rows(name, result?, alias, column_aliases);
        }
    }
    let record_definition = (!column_types.is_empty()).then_some((column_aliases, column_types));
    let user_result = match binding {
        Some(binding) if binding.builtin => None,
        None => invocation::call_user_table_function(
            &context.routines,
            &identity,
            &call_args,
            record_definition,
        ),
        Some(binding) => invocation::call_bound_user_table_function(
            &context.routines,
            binding,
            &call_args,
            record_definition,
        ),
    };
    if let Some(result) = user_result {
        return registered_table_function_rows(name, result?, alias, column_aliases);
    }
    if let Some(binding) = binding.filter(|binding| !binding.builtin) {
        return Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!(
                "bound function {}({}) does not exist",
                binding.name,
                binding.argument_types.join(", ")
            ),
        });
    }
    if has_named_args {
        return Err(unknown_function_error(&lower, &call_args));
    }
    match lower.as_str() {
        "pg_get_sequence_data" => singleton_record_rows(
            context.session.sequence_data(&evaluated)?,
            &["last_value", "is_called"],
            column_aliases,
        ),
        "pg_sequence_parameters" => singleton_record_rows(
            context.session.sequence_parameters(&evaluated)?,
            &[
                "start_value",
                "minimum_value",
                "maximum_value",
                "increment",
                "cycle_option",
                "cache_size",
                "data_type",
            ],
            column_aliases,
        ),
        "create_analyzer" | "drop_analyzer" | "list_analyzers" | "fts_index_stats"
        | "set_table_analyzer" => {
            super::analyzers::build_rows(context.analyzers, &lower, &evaluated, column_aliases)
        }
        "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
        | "graph_betweenness" | "cypher" | "rpq" => {
            super::graphs::build_rows(context, call, &lower, &evaluated)
        }
        other => Err(SQLError::Unsupported(format!(
            "table function `{other}` in FROM"
        ))),
    }
}
