//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lazy table-function evaluation, host callback streams, cancellation, and ordinality.

use super::{
    context::TableFunctionContext,
    dispatch::build_table_function_rows_with_row,
    registered_table_function_row_stream,
    values::{
        generate_series_values, json_array_values, json_each_row_stream, json_object_key_values,
        regexp_split_values, string_to_table_values, unnest_row_stream,
    },
    TableFunctionCall, TableFunctionRows,
};
use crate::scalar::plan::PlanSubqueryArena;
use crate::{eval_call_arguments, ScalarEvalContext};
use uqa_core::Value;
use uqa_sql::{semantics::scalar_table_function_default_column, SQLError};

/// Build a table-function result as a fallible owned row stream. Built-in cardinality-producing functions are evaluated lazily; registered/user functions keep their existing vector-valued API and are adapted at this explicit extension boundary. A correlated lateral caller supplies its physical outer row so function arguments are evaluated in the same scope used during binding.
#[allow(clippy::similar_names)]
pub fn build_table_function_row_stream_with_row(
    context: &TableFunctionContext<'_>,
    call: TableFunctionCall<'_>,
    row: Option<&crate::OwnedPhysicalRow>,
) -> Result<TableFunctionRows, SQLError> {
    let ordinality = call.ordinality;
    let mut output = build_table_function_value_row_stream_with_row(context, call, row)?;
    let cancellation = context.runtime.cancellation_token();
    let mut rows = output.rows;
    let mut cancelled = false;
    output.rows = Box::new(std::iter::from_fn(move || {
        if cancelled {
            return None;
        }
        if let Err(error) = cancellation.check() {
            cancelled = true;
            return Some(Err(SQLError::from(error).into()));
        }
        rows.next()
    }));
    if !ordinality {
        return Ok(output);
    }
    output.columns.push(
        call.column_aliases
            .get(output.columns.len())
            .cloned()
            .unwrap_or_else(|| "ordinality".into()),
    );
    let mut next = Some(1_i64);
    output.rows = Box::new(output.rows.map(move |row| {
        let row = row?;
        let ordinal = next.ok_or_else(|| {
            crate::ExecError::SQL(SQLError::Routine {
                sqlstate: "22003".into(),
                message: "WITH ORDINALITY counter exceeds bigint".into(),
            })
        })?;
        next = ordinal.checked_add(1);
        Ok(row.append_values(vec![Value::Int(ordinal)]))
    }));
    Ok(output)
}

#[allow(clippy::similar_names)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
fn build_table_function_value_row_stream_with_row(
    context: &TableFunctionContext<'_>,
    call: TableFunctionCall<'_>,
    row: Option<&crate::OwnedPhysicalRow>,
) -> Result<TableFunctionRows, SQLError> {
    let TableFunctionCall {
        name,
        binding,
        output_name,
        args,
        alias,
        column_aliases,
        ordinality,
        ..
    } = call;
    let identity = name.to_ascii_lowercase();
    let lower = uqa_sql::semantics::builtin_function_dispatch_name(&identity);
    if binding.is_none_or(|binding| binding.builtin)
        && matches!(
            lower.as_str(),
            "generate_series"
                | "pg_listening_channels"
                | "unnest"
                | "regexp_split_to_table"
                | "string_to_table"
                | "json_array_elements"
                | "jsonb_array_elements"
                | "json_array_elements_text"
                | "jsonb_array_elements_text"
                | "json_object_keys"
                | "jsonb_object_keys"
                | "json_each"
                | "jsonb_each"
                | "json_each_text"
                | "jsonb_each_text"
        )
    {
        let subquery_arena =
            PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
        let scalar_context = match row {
            Some(row) => ScalarEvalContext::from_row_lookup(row, context.params)
                .with_physical_outer_row(&row.schema, &row.row),
            None => ScalarEvalContext::new(None, context.params),
        };
        let scalar_context = scalar_context
            .with_function_hook(context.eval_hook)
            .with_subquery_runner(&subquery_arena);
        let call_args = eval_call_arguments(args, &scalar_context)?;
        if call_args.iter().any(|(name, _)| name.is_some()) {
            return Err(uqa_sql::expr::unknown_function_error(&lower, &call_args));
        }
        let evaluated: Vec<Value> = call_args.into_iter().map(|(_, value)| value).collect();
        if lower == "unnest" {
            let aliases = if ordinality {
                &column_aliases[..column_aliases.len().min(evaluated.len())]
            } else {
                column_aliases
            };
            return unnest_row_stream(evaluated, output_name, alias, aliases);
        }
        let default_col =
            scalar_table_function_default_column(&lower, output_name, alias, column_aliases);

        let values: Box<dyn Iterator<Item = Value> + Send> = match lower.as_str() {
            "pg_listening_channels" => {
                uqa_sql::semantics::table_function_arguments::require_no_arguments(
                    &lower, &evaluated,
                )?;
                Box::new(
                    context
                        .session
                        .listening_channels()
                        .into_iter()
                        .map(Value::Str),
                )
            }
            "generate_series" => generate_series_values(evaluated)?,
            "regexp_split_to_table" => regexp_split_values(evaluated)?,
            "string_to_table" => string_to_table_values(evaluated)?,
            "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text" => json_array_values(&lower, evaluated)?,
            "json_object_keys" | "jsonb_object_keys" => json_object_key_values(&lower, evaluated)?,
            "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
                return json_each_row_stream(&lower, evaluated, alias, column_aliases);
            }
            _ => {
                return Err(SQLError::Internal(format!(
                    "streaming table function `{lower}` reached an unsupported dispatch branch"
                )));
            }
        };
        return Ok(TableFunctionRows::new(
            vec![default_col],
            Box::new(values.map(|value| Ok(crate::PhysicalRow::from_values(vec![value])))),
        ));
    }

    if binding.is_none_or(|binding| binding.builtin)
        && context.runtime.has_table_function(&identity)
    {
        let subquery_arena =
            PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
        let scalar_context = match row {
            Some(row) => ScalarEvalContext::from_row_lookup(row, context.params)
                .with_physical_outer_row(&row.schema, &row.row),
            None => ScalarEvalContext::new(None, context.params),
        };
        let scalar_context = scalar_context
            .with_function_hook(context.eval_hook)
            .with_subquery_runner(&subquery_arena);
        let call_args = eval_call_arguments(args, &scalar_context)?;
        if call_args.iter().any(|(name, _)| name.is_some()) {
            return Err(uqa_sql::expr::unknown_function_error(&lower, &call_args));
        }
        let evaluated = call_args
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>();
        let result = context
            .runtime
            .lookup_table_function(&identity)
            .map(|registration| registration.function.call_stream(&evaluated))
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "registered table function `{name}` disappeared during execution"
                ))
            })??;
        return registered_table_function_row_stream(name, result, alias, column_aliases);
    }

    build_table_function_rows_with_row(context, call, row)
}
