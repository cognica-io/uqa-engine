//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table-function evaluation context, stream entry point, and scalar row shaping.

use super::{
    build_table_function_rows_with_row, eval_call_arguments, generate_series_values,
    is_json_array_table_function, json_array_values, json_each_row_stream, prefix_row,
    regexp_split_values, registered_table_function_row_stream,
    scalar_table_function_default_column, string_to_table_values, Engine, PhysicalSubqueryRunner,
    PlanSubqueryArena, QueryPlan, ResultRow, SQLError, SQLParam, ScalarEvalContext, ScalarExpr,
    Value,
};

pub(in crate::sql) struct TableFunctionEvalContext<'a> {
    pub(super) engine: &'a Engine,
    pub(super) params: &'a [SQLParam],
    pub(super) eval_hook: &'a dyn uqa_sql::expr::EngineHook,
    pub(super) subquery_runner: &'a dyn PhysicalSubqueryRunner,
    pub(super) subqueries: &'a [QueryPlan],
}

impl<'a> TableFunctionEvalContext<'a> {
    pub(in crate::sql) fn new(
        engine: &'a Engine,
        params: &'a [SQLParam],
        eval_hook: &'a dyn uqa_sql::expr::EngineHook,
        subquery_runner: &'a dyn PhysicalSubqueryRunner,
        subqueries: &'a [QueryPlan],
    ) -> Self {
        Self {
            engine,
            params,
            eval_hook,
            subquery_runner,
            subqueries,
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::sql) struct TableFunctionCall<'a> {
    pub(super) name: &'a str,
    pub(super) relation: Option<&'a str>,
    pub(super) args: &'a [ScalarExpr],
    pub(super) alias: Option<&'a str>,
    pub(super) column_aliases: &'a [String],
    pub(super) column_types: &'a [String],
}

impl<'a> TableFunctionCall<'a> {
    pub(in crate::sql) fn new(
        name: &'a str,
        relation: Option<&'a str>,
        args: &'a [ScalarExpr],
        alias: Option<&'a str>,
        column_aliases: &'a [String],
        column_types: &'a [String],
    ) -> Self {
        Self {
            name,
            relation,
            args,
            alias,
            column_aliases,
            column_types,
        }
    }
}

/// Build a table-function result as a fallible owned row stream. Built-in
/// cardinality-producing functions are evaluated lazily; registered/user
/// functions keep their existing vector-valued API and are adapted at this
/// explicit extension boundary.
pub(in crate::sql) fn build_table_function_row_stream(
    context: &TableFunctionEvalContext<'_>,
    call: TableFunctionCall<'_>,
) -> Result<uqa_execution::ProjectRows, SQLError> {
    build_table_function_row_stream_with_row(context, call, None)
}

#[allow(clippy::similar_names)]
pub(in crate::sql) fn build_table_function_row_stream_with_row(
    context: &TableFunctionEvalContext<'_>,
    call: TableFunctionCall<'_>,
    row: Option<&ResultRow>,
) -> Result<uqa_execution::ProjectRows, SQLError> {
    let TableFunctionCall {
        name,
        args,
        alias,
        column_aliases,
        ..
    } = call;
    let identity = name.to_ascii_lowercase();
    let lower = crate::sql::builtin_function_dispatch_name(&identity);
    if matches!(
        lower.as_str(),
        "generate_series"
            | "unnest"
            | "regexp_split_to_table"
            | "string_to_table"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
    ) {
        let subquery_arena =
            PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
        let scalar_context = ScalarEvalContext::new(row, context.params)
            .with_function_hook(context.eval_hook)
            .with_subquery_runner(&subquery_arena);
        let call_args = eval_call_arguments(args, &scalar_context)?;
        if call_args.iter().any(|(name, _)| name.is_some()) {
            return Err(uqa_sql::expr::unknown_function_error(&lower, &call_args));
        }
        let evaluated: Vec<Value> = call_args.into_iter().map(|(_, value)| value).collect();
        let json_array_function = is_json_array_table_function(&lower);
        let default_col = scalar_table_function_default_column(&lower, alias, column_aliases);
        let row_builder = ScalarFunctionRowBuilder {
            default_col,
            function_name: lower.clone(),
            qualifier: alias.map(str::to_string),
            preserve_function_name: !json_array_function
                && column_aliases.is_empty()
                && alias.is_some(),
        };

        let values: Box<dyn Iterator<Item = Value> + Send> = match lower.as_str() {
            "generate_series" => generate_series_values(evaluated)?,
            "unnest" => Box::new(evaluated.into_iter().flat_map(|value| match value {
                Value::List(items) => items,
                value => vec![value],
            })),
            "regexp_split_to_table" => regexp_split_values(evaluated)?,
            "string_to_table" => string_to_table_values(evaluated)?,
            "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text" => json_array_values(&lower, evaluated)?,
            "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
                return json_each_row_stream(&lower, evaluated, alias, column_aliases);
            }
            _ => {
                return Err(SQLError::Internal(format!(
                    "streaming table function `{lower}` reached an unsupported dispatch branch"
                )));
            }
        };
        return Ok(Box::new(
            values.map(move |value| Ok(row_builder.row(value))),
        ));
    }

    if context.engine.has_registered_table_function(&identity) {
        let subquery_arena =
            PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
        let scalar_context = ScalarEvalContext::new(row, context.params)
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
            .engine
            .call_registered_table_function_stream(&identity, &evaluated)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "registered table function `{name}` disappeared during execution"
                ))
            })??;
        return registered_table_function_row_stream(name, result, alias, column_aliases);
    }

    let rows = build_table_function_rows_with_row(context, call, row)?;
    Ok(Box::new(rows.into_iter().map(Ok)))
}

#[derive(Clone)]
pub(in crate::sql) struct ScalarFunctionRowBuilder {
    default_col: String,
    function_name: String,
    qualifier: Option<String>,
    preserve_function_name: bool,
}

impl ScalarFunctionRowBuilder {
    fn row(&self, value: Value) -> ResultRow {
        let mut row = ResultRow::new();
        row.insert(self.default_col.clone(), value.clone());
        if self.preserve_function_name && self.default_col != self.function_name {
            row.insert(self.function_name.clone(), value);
        }
        self.qualifier
            .as_deref()
            .map_or(row.clone(), |qualifier| prefix_row(qualifier, &row))
    }
}
