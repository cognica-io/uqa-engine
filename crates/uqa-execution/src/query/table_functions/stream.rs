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
        generate_series_values, json_array_values, json_each_rows, json_object_key_values,
        string_to_table_values, unnest_rows,
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
    if let Some(binding) = call.binding {
        context.eval_hook.require_builtin_execute(binding)?;
    }
    let ordinality = call.ordinality;
    let mut output = build_table_function_value_row_stream_with_row(context, call, row)?;
    let cancellation = context.runtime.cancellation_token();
    output.rows = crate::SQLRowStream::from_source(CancellableRows {
        rows: output.rows,
        cancellation,
    });
    if !ordinality {
        return Ok(output);
    }
    output
        .columns
        .push(ordinality_column(&call, output.columns.len()));
    let mut next = Some(1_i64);
    output.rows = output.rows.map_rows(move |row| {
        let ordinal = next.ok_or_else(|| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "WITH ORDINALITY counter exceeds bigint".into(),
        })?;
        next = ordinal.checked_add(1);
        Ok(row.append_values(vec![Value::Int(ordinal)]))
    });
    Ok(output)
}

struct CancellableRows {
    rows: crate::PhysicalProjectRows,
    cancellation: uqa_core::CancellationToken,
}

impl crate::SQLRowSource<crate::PhysicalRow> for CancellableRows {
    fn next_row(&mut self) -> Result<Option<crate::PhysicalRow>, SQLError> {
        self.cancellation.check()?;
        self.rows.next().transpose()
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.rows.close()
    }
}

/// The output columns of a built-in set-returning call, including its ordinality column. They follow from the call's name, aliases and argument count, never from argument values, so a FROM item can describe its rows before it evaluates the call, as `PostgreSQL` does until the first row is requested. `None` for a function whose columns only its result reveals.
pub fn deferred_table_function_columns(
    call: &TableFunctionCall<'_>,
) -> Option<Result<Vec<String>, SQLError>> {
    let lower = streaming_builtin(call)?;
    Some(builtin_value_columns(&lower, call).map(|mut columns| {
        if call.ordinality {
            let column = ordinality_column(call, columns.len());
            columns.push(column);
        }
        columns
    }))
}

fn ordinality_column(call: &TableFunctionCall<'_>, position: usize) -> String {
    call.column_aliases
        .get(position)
        .cloned()
        .unwrap_or_else(|| "ordinality".into())
}

/// The dispatch name of a built-in set-returning function that this module streams.
fn streaming_builtin(call: &TableFunctionCall<'_>) -> Option<String> {
    if call.binding.is_some_and(|binding| !binding.builtin) {
        return None;
    }
    let lower = uqa_sql::semantics::builtin_function_dispatch_name(&call.name.to_ascii_lowercase());
    matches!(
        lower.as_str(),
        "generate_series"
            | "pg_listening_channels"
            | "unnest"
            | "regexp_matches"
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
            | "aclexplode"
    )
    .then_some(lower)
}

/// The value columns of a streaming built-in, before any ordinality column.
fn builtin_value_columns(
    lower: &str,
    call: &TableFunctionCall<'_>,
) -> Result<Vec<String>, SQLError> {
    Ok(match lower {
        "unnest" => {
            let aliases = if call.ordinality {
                &call.column_aliases[..call.column_aliases.len().min(call.args.len())]
            } else {
                call.column_aliases
            };
            if call.args.is_empty() || aliases.len() > call.args.len() {
                return Err(SQLError::BadArity {
                    name: "unnest".into(),
                    expected: "one output alias per array argument".into(),
                    actual: call.args.len(),
                });
            }
            uqa_sql::semantics::table_function_empty_schema(
                "unnest",
                call.output_name,
                call.alias,
                aliases,
                call.args.len(),
                false,
            )
        }
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => vec![
            call.column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "key".into()),
            call.column_aliases
                .get(1)
                .cloned()
                .unwrap_or_else(|| "value".into()),
        ],
        "aclexplode" => super::acl::aclexplode_columns(call.column_aliases),
        _ => vec![scalar_table_function_default_column(
            lower,
            call.output_name,
            call.alias,
            call.column_aliases,
        )],
    })
}

#[allow(clippy::similar_names)]
fn build_table_function_value_row_stream_with_row(
    context: &TableFunctionContext<'_>,
    call: TableFunctionCall<'_>,
    row: Option<&crate::OwnedPhysicalRow>,
) -> Result<TableFunctionRows, SQLError> {
    if let Some(lower) = streaming_builtin(&call) {
        let call_args = evaluate_arguments(context, &call, row)?;
        let empty = crate::RowSchema::default();
        let selected = if call.binding.is_none() {
            uqa_sql::semantics::resolve_runtime_table_function_binding(
                context.routines.lookup,
                call.name,
                call.args,
                &call_args,
                row.map_or(&empty, |row| &row.schema),
                context.params,
                context.resolver,
            )?
        } else {
            None
        };
        let binding = selected.as_ref().or(call.binding);
        if let Some(binding) = binding.filter(|binding| !binding.builtin) {
            let record =
                (!call.column_types.is_empty()).then_some((call.column_aliases, call.column_types));
            let result = crate::routines::invocation::call_bound_user_table_function(
                &context.routines,
                binding,
                &call_args,
                record,
            )
            .ok_or_else(|| SQLError::Internal("selected table function disappeared".into()))??;
            return super::registered_table_function_rows(
                call.name,
                result,
                call.alias,
                call.column_aliases,
            );
        }
        if let Some(binding) = selected.as_ref() {
            context.eval_hook.require_builtin_execute(binding)?;
        }
        let columns = builtin_value_columns(&lower, &call)?;
        let evaluated = builtin_arguments(&lower, call_args)?;
        // The regular-expression functions read the call's input collation, `PostgreSQL`'s `inputcollid`.
        let collation = uqa_sql::ast::FunctionBinding::selected_input_collation(binding);
        let rows = builtin_value_rows(context, &lower, evaluated, collation, &columns)?;
        return Ok(TableFunctionRows::new(columns, rows));
    }
    let TableFunctionCall {
        name,
        binding,
        alias,
        column_aliases,
        args,
        ..
    } = call;
    let identity = name.to_ascii_lowercase();
    let lower = uqa_sql::semantics::builtin_function_dispatch_name(&identity);
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

/// Evaluate arguments once in their calling scope before choosing a dynamically typed overload.
fn evaluate_arguments(
    context: &TableFunctionContext<'_>,
    call: &TableFunctionCall<'_>,
    row: Option<&crate::OwnedPhysicalRow>,
) -> Result<Vec<(Option<String>, Value)>, SQLError> {
    let subquery_arena = PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
    let scalar_context = match row {
        Some(row) => ScalarEvalContext::from_row_lookup(row, context.params)
            .with_physical_outer_row(&row.schema, &row.row),
        None => ScalarEvalContext::new(None, context.params),
    };
    let mut scalar_context = scalar_context
        .with_function_hook(context.eval_hook)
        .with_subquery_runner(&subquery_arena);
    if let Some(states) = call.function_states {
        scalar_context = scalar_context.with_function_states(states);
    }
    // Arguments bind like any other expression: catalog-typed calls such as `enum_range(NULL::mood)` select their operation from declared types.
    let empty = crate::RowSchema::default();
    let schema = row.map_or(&empty, |row| &row.schema);
    let bound = call.function_states.is_none().then(|| {
        call.args
            .iter()
            .map(|argument| {
                uqa_sql::bind_type_introspection_with_resolver(
                    argument.clone(),
                    schema,
                    context.params,
                    context.resolver,
                )
            })
            .collect::<Vec<_>>()
    });
    eval_call_arguments(bound.as_deref().unwrap_or(call.args), &scalar_context)
}

fn builtin_arguments(
    lower: &str,
    call_args: Vec<(Option<String>, Value)>,
) -> Result<Vec<Value>, SQLError> {
    // Named arguments take their declared positions as in a scalar call; a function without such a signature is undefined.
    if call_args.iter().any(|(name, _)| name.is_some()) {
        return Ok(uqa_sql::expr::builtin_named_args(
            lower,
            &call_args,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )?
        .ok_or_else(|| uqa_sql::expr::unknown_function_error(lower, &call_args))?
        .into_parts()
        .0);
    }
    Ok(call_args.into_iter().map(|(_, value)| value).collect())
}

/// The rows of a streaming built-in over its evaluated arguments and input collation, in the layout `columns` names.
fn builtin_value_rows(
    context: &TableFunctionContext<'_>,
    lower: &str,
    evaluated: Vec<Value>,
    collation: u32,
    columns: &[String],
) -> Result<crate::PhysicalProjectRows, SQLError> {
    let values: Box<dyn Iterator<Item = Value> + Send> = match lower {
        "unnest" => return unnest_rows(evaluated),
        "pg_listening_channels" => {
            uqa_sql::semantics::table_function_arguments::require_no_arguments(lower, &evaluated)?;
            Box::new(
                context
                    .session
                    .listening_channels()
                    .into_iter()
                    .map(Value::Str),
            )
        }
        "generate_series" => generate_series_values(evaluated)?,
        "regexp_matches" => {
            return super::regular_expressions::regexp_matches_row_stream(
                &evaluated,
                collation,
                columns[0].clone(),
            )
            .map(|output| output.rows);
        }
        "regexp_split_to_table" => {
            return super::regular_expressions::regexp_split_to_table_row_stream(
                &evaluated,
                collation,
                columns[0].clone(),
            )
            .map(|output| output.rows);
        }
        "string_to_table" => string_to_table_values(evaluated)?,
        "json_array_elements"
        | "jsonb_array_elements"
        | "json_array_elements_text"
        | "jsonb_array_elements_text" => json_array_values(lower, evaluated)?,
        "json_object_keys" | "jsonb_object_keys" => json_object_key_values(lower, evaluated)?,
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
            return json_each_rows(lower, evaluated);
        }
        "aclexplode" => {
            return super::acl::aclexplode_rows(&|name| context.session.role_oid(name), evaluated);
        }
        _ => {
            return Err(SQLError::Internal(format!(
                "streaming table function `{lower}` reached an unsupported dispatch branch"
            )));
        }
    };
    Ok(crate::SQLRowStream::new(values.map(|value| {
        Ok(crate::PhysicalRow::from_values(vec![value]))
    })))
}
