//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in and registered table-function row dispatch.

use super::{
    age_cypher, checked_integer_value, doc_id_value, eval_call_arguments, execute_tree_entries,
    expect_optional_graph_value, generate_series_values, graph_betweenness_entries,
    graph_hits_entries, graph_pagerank_entries, json_table_arg, json_table_value_to_text,
    unnest_row_stream, PlanSubqueryArena, SQLError, SQLTableFunctionResult, SQLTableFunctionStream,
    ScalarEvalContext, SourceEvalContext, TableFunctionCall, TableFunctionRows, Value,
};

#[allow(clippy::similar_names)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub(in crate::sql) fn build_table_function_rows_with_row(
    context: &SourceEvalContext<'_>,
    call: TableFunctionCall<'_>,
    row: Option<&uqa_execution::OwnedPhysicalRow>,
) -> Result<TableFunctionRows, SQLError> {
    let TableFunctionCall {
        name,
        binding,
        output_name,
        relations,
        args,
        alias,
        column_aliases,
        column_types,
        ..
    } = call;
    use uqa_sql::expr::unknown_function_error;
    let engine = context.engine;
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
    let lower = crate::sql::builtin_function_dispatch_name(&identity);
    if binding.is_none_or(|binding| binding.builtin)
        && crate::operator_tree_bridge::is_operator_join_table_function(&lower)
    {
        let tuples = crate::operator_tree_bridge::execute_operator_join_table_function(
            engine,
            &lower,
            relations,
            args,
            context.params,
        )?;
        return operator_join_rows(tuples, alias, column_aliases);
    }
    let call_args = eval_call_arguments(args, &ctx)?;
    let has_named_args = call_args.iter().any(|(name, _)| name.is_some());
    let evaluated: Vec<Value> = call_args.iter().map(|(_, value)| value.clone()).collect();
    let default_col = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| alias.unwrap_or(output_name).to_string());
    let mut out: Vec<Vec<Value>> = Vec::new();
    let push_scalar = |out: &mut Vec<Vec<Value>>, value: Value| out.push(vec![value]);
    if !has_named_args && binding.is_none_or(|binding| binding.builtin) {
        if let Some(result) = engine.call_registered_table_function(&identity, &evaluated) {
            return registered_table_function_rows(name, result?, alias, column_aliases);
        }
    }
    let record_definition = (!column_types.is_empty()).then_some((column_aliases, column_types));
    let user_result = match binding {
        Some(binding) if binding.builtin => None,
        None => crate::sql::plpgsql_exec::call_user_table_function(
            engine,
            &identity,
            &call_args,
            record_definition,
        ),
        Some(binding) => crate::sql::plpgsql_exec::call_bound_user_table_function(
            engine,
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
        "pg_listening_channels" => {
            if !evaluated.is_empty() {
                return Err(SQLError::BadArity {
                    name: lower,
                    expected: "0".into(),
                    actual: evaluated.len(),
                });
            }
            for channel in engine.listening_channels() {
                push_scalar(&mut out, Value::Str(channel));
            }
            Ok(TableFunctionRows::materialized(vec![default_col], out))
        }
        "generate_series" => {
            for value in generate_series_values(evaluated)? {
                push_scalar(&mut out, value);
            }
            Ok(TableFunctionRows::materialized(vec![default_col], out))
        }
        "unnest" => unnest_row_stream(evaluated, output_name, alias, column_aliases),
        "regexp_split_to_table" => {
            if evaluated.len() != 2 {
                return Err(SQLError::TypeMismatch(
                    "regexp_split_to_table requires 2 args".into(),
                ));
            }
            let s = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 1".into())),
            };
            let pat = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 2".into())),
            };
            let re = regex::Regex::new(&pat)
                .map_err(|e| SQLError::TypeMismatch(format!("invalid regex: {e}")))?;
            for piece in re.split(&s) {
                push_scalar(&mut out, Value::Str(piece.to_string()));
            }
            Ok(TableFunctionRows::materialized(vec![default_col], out))
        }
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let parsed = json_table_arg(&evaluated[0], &lower)?;
            let serde_json::Value::Object(obj) = parsed else {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower}: argument is not an object"
                )));
            };
            let key_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "key".into());
            let val_col = column_aliases
                .get(1)
                .cloned()
                .unwrap_or_else(|| "value".into());
            for (k, v) in obj {
                out.push(vec![Value::Str(k), json_table_value_to_text(&v)]);
            }
            Ok(TableFunctionRows::materialized(vec![key_col, val_col], out))
        }
        "json_array_elements"
        | "jsonb_array_elements"
        | "json_array_elements_text"
        | "jsonb_array_elements_text" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let parsed = json_table_arg(&evaluated[0], &lower)?;
            let serde_json::Value::Array(arr) = parsed else {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower}: argument is not an array"
                )));
            };
            let col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "value".into());
            for v in arr {
                out.push(vec![json_table_value_to_text(&v)]);
            }
            Ok(TableFunctionRows::materialized(vec![col], out))
        }
        "pg_get_sequence_data" => singleton_record_rows(
            engine.pg_get_sequence_data_value(&evaluated)?,
            &["last_value", "is_called"],
            column_aliases,
        ),
        "pg_sequence_parameters" => singleton_record_rows(
            engine.pg_sequence_parameters_value(&evaluated)?,
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
        // -------------------------------------------------------------
        // Analyzer DDL exposed as table functions: create, drop, list,
        // and assign table analyzers.
        // -------------------------------------------------------------
        "create_analyzer" => {
            if evaluated.len() != 2 {
                return Err(SQLError::BadArity {
                    name: "create_analyzer".into(),
                    expected: "2".into(),
                    actual: evaluated.len(),
                });
            }
            let analyzer_name = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("create_analyzer arg 1".into())),
            };
            let config_json = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("create_analyzer arg 2".into())),
            };
            engine
                .register_named_analyzer(&analyzer_name, &config_json)
                .map_err(SQLError::Unsupported)?;
            let column = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "create_analyzer".into());
            Ok(TableFunctionRows::materialized(
                vec![column],
                vec![vec![Value::Str(format!(
                    "analyzer '{analyzer_name}' created"
                ))]],
            ))
        }
        "drop_analyzer" => {
            if evaluated.len() != 1 {
                return Err(SQLError::BadArity {
                    name: "drop_analyzer".into(),
                    expected: "1".into(),
                    actual: evaluated.len(),
                });
            }
            let analyzer_name = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("drop_analyzer arg 1".into())),
            };
            let removed = engine
                .drop_named_analyzer(&analyzer_name)
                .map_err(SQLError::Internal)?;
            if !removed {
                return Err(SQLError::Unsupported(format!(
                    "analyzer `{analyzer_name}` does not exist"
                )));
            }
            let column = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "drop_analyzer".into());
            Ok(TableFunctionRows::materialized(
                vec![column],
                vec![vec![Value::Str(format!(
                    "analyzer '{analyzer_name}' dropped"
                ))]],
            ))
        }
        "list_analyzers" => {
            if !evaluated.is_empty() {
                return Err(SQLError::BadArity {
                    name: "list_analyzers".into(),
                    expected: "0".into(),
                    actual: evaluated.len(),
                });
            }
            // Include the four built-in analyzers (`whitespace`, `standard`,
            // `standard_cjk`, `keyword`) in addition to
            // top of every user-registered named analyzer.
            let mut names: std::collections::BTreeSet<String> = engine
                .list_named_analyzers()
                .map_err(SQLError::Unsupported)?
                .into_iter()
                .collect();
            for builtin in ["whitespace", "standard", "standard_cjk", "keyword"] {
                names.insert(builtin.to_string());
            }
            let key = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "analyzer_name".into());
            for n in names {
                out.push(vec![Value::Str(n)]);
            }
            Ok(TableFunctionRows::materialized(vec![key], out))
        }
        "fts_index_stats" => {
            if evaluated.len() > 1 {
                return Err(SQLError::TypeMismatch(
                    "fts_index_stats accepts optional table name".into(),
                ));
            }
            let table_filter = match evaluated.first() {
                Some(Value::Str(s)) => Some(s.as_str()),
                Some(_) => return Err(SQLError::TypeMismatch("fts_index_stats arg 1".into())),
                None => None,
            };
            for stat in engine.fts_index_stats(table_filter)? {
                out.push(vec![
                    Value::Str(stat.table_name),
                    Value::Str(stat.field),
                    Value::Str(stat.analyzer),
                    checked_integer_value(stat.posting_count, "posting count")?,
                    checked_integer_value(stat.doc_length_count, "document-length count")?,
                    checked_integer_value(stat.indexed_doc_count, "indexed-document count")?,
                    checked_integer_value(stat.term_count, "term count")?,
                    checked_integer_value(stat.total_field_length, "total field length")?,
                ]);
            }
            Ok(TableFunctionRows::materialized(
                vec![
                    "table_name".into(),
                    "field".into(),
                    "analyzer".into(),
                    "posting_count".into(),
                    "doc_length_count".into(),
                    "indexed_doc_count".into(),
                    "term_count".into(),
                    "total_field_length".into(),
                ],
                out,
            ))
        }
        "set_table_analyzer" => {
            if !(3..=4).contains(&evaluated.len()) {
                return Err(SQLError::BadArity {
                    name: "set_table_analyzer".into(),
                    expected: "3 or 4".into(),
                    actual: evaluated.len(),
                });
            }
            let target_table = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 1".into())),
            };
            let field = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 2".into())),
            };
            let analyzer_name = match &evaluated[2] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 3".into())),
            };
            let phase = if evaluated.len() > 3 {
                match &evaluated[3] {
                    Value::Str(s) => s.clone(),
                    _ => {
                        return Err(SQLError::TypeMismatch(
                            "set_table_analyzer phase must be a string".into(),
                        ));
                    }
                }
            } else {
                "both".into()
            };
            engine
                .set_table_field_analyzer(&target_table, &field, &analyzer_name, &phase)
                .map_err(SQLError::Unsupported)?;
            let mut msg = format!("analyzer '{analyzer_name}' assigned to {target_table}.{field}");
            if phase != "both" {
                use std::fmt::Write as _;
                let _ = write!(msg, " (phase={phase})");
            }
            let column = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "set_table_analyzer".into());
            Ok(TableFunctionRows::materialized(
                vec![column],
                vec![vec![Value::Str(msg)]],
            ))
        }
        "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
        | "graph_betweenness" => {
            if evaluated.len() > 1 {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower} accepts at most one graph argument"
                )));
            }
            let graph = expect_optional_graph_value(engine, evaluated.first(), &lower)?;
            let entries = match lower.as_str() {
                "pagerank" | "graph_pagerank" => graph_pagerank_entries(engine, &graph)?,
                "hits" | "graph_hits" => graph_hits_entries(engine, &graph)?,
                "betweenness" | "graph_betweenness" => graph_betweenness_entries(engine, &graph)?,
                _ => {
                    return Err(SQLError::Internal(format!(
                        "graph centrality function `{lower}` reached an unsupported dispatch branch"
                    )));
                }
            };
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "_doc_id".into());
            let score_col = column_aliases
                .get(1)
                .cloned()
                .unwrap_or_else(|| "_score".into());
            for entry in entries {
                out.push(vec![doc_id_value(entry.doc_id)?, Value::Float(entry.score)]);
            }
            Ok(TableFunctionRows::materialized(
                vec![id_col, score_col],
                out,
            ))
        }
        "cypher" => Ok(TableFunctionRows::materialized(
            column_aliases.to_vec(),
            age_cypher::build_rows(
                engine,
                args,
                &evaluated,
                alias,
                column_aliases,
                column_types,
            )?,
        )),
        "rpq" => {
            if !(2..=3).contains(&evaluated.len()) {
                return Err(SQLError::TypeMismatch(
                    "rpq requires 2 or 3 args (expr, start [, graph])".into(),
                ));
            }
            let expr_str = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("rpq.expr must be string".into())),
            };
            let start = match &evaluated[1] {
                Value::Int(n) => u64::try_from(*n).map_err(|_| {
                    SQLError::TypeMismatch("rpq.start must be a non-negative integer".into())
                })?,
                _ => return Err(SQLError::TypeMismatch("rpq.start must be integer".into())),
            };
            let graph = expect_optional_graph_value(engine, evaluated.get(2), "rpq")?;
            let entries = execute_tree_entries(
                engine,
                &uqa_operators::OperatorTree::RegularPathQuery {
                    rpq_source: expr_str,
                    start_vertex: start,
                    graph,
                },
            )?;
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "vertex_id".into());
            for entry in entries {
                out.push(vec![doc_id_value(entry.doc_id)?]);
            }
            Ok(TableFunctionRows::materialized(vec![id_col], out))
        }
        other => Err(SQLError::Unsupported(format!(
            "table function `{other}` in FROM"
        ))),
    }
}

fn singleton_record_rows(
    value: Value,
    default_columns: &[&str],
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    let columns = default_columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            column_aliases
                .get(index)
                .cloned()
                .unwrap_or_else(|| (*column).into())
        })
        .collect::<Vec<_>>();
    let values = match value {
        Value::Null => vec![Value::Null; columns.len()],
        Value::Record(fields) if fields.len() == columns.len() => {
            fields.into_iter().map(|(_, value)| value).collect()
        }
        Value::Record(fields) => {
            return Err(SQLError::Internal(format!(
                "sequence introspection returned {} fields for {} columns",
                fields.len(),
                columns.len()
            )));
        }
        value => {
            return Err(SQLError::Internal(format!(
                "sequence introspection returned non-record value {value:?}"
            )));
        }
    };
    Ok(TableFunctionRows::materialized(columns, vec![values]))
}

fn operator_join_rows(
    tuples: uqa_core::GeneralizedPostingList,
    _alias: Option<&str>,
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    let left_column = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| "left_doc_id".into());
    let right_column = column_aliases
        .get(1)
        .cloned()
        .unwrap_or_else(|| "right_doc_id".into());
    let score_column = column_aliases
        .get(2)
        .cloned()
        .unwrap_or_else(|| "_score".into());
    let mut rows = Vec::with_capacity(tuples.len());
    for tuple in tuples.entries() {
        let [left_doc_id, right_doc_id] = tuple.doc_ids.as_slice() else {
            return Err(SQLError::Internal(format!(
                "operator join produced a {}-element tuple; SQL table joins require pairs",
                tuple.doc_ids.len()
            )));
        };
        rows.push(vec![
            doc_id_value(*left_doc_id)?,
            doc_id_value(*right_doc_id)?,
            tuple
                .payload
                .fields
                .get("_score")
                .cloned()
                .unwrap_or(Value::Null),
        ]);
    }
    Ok(TableFunctionRows::materialized(
        vec![left_column, right_column, score_column],
        rows,
    ))
}

pub(in crate::sql) fn registered_table_function_rows(
    name: &str,
    result: SQLTableFunctionResult,
    _alias: Option<&str>,
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    if result.columns.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "table function `{name}` returned no columns"
        )));
    }
    let columns: Vec<String> = result
        .columns
        .iter()
        .enumerate()
        .map(|(idx, column)| {
            column_aliases
                .get(idx)
                .cloned()
                .unwrap_or_else(|| column.clone())
        })
        .collect();
    let mut out = Vec::with_capacity(result.rows.len());
    for values in result.rows {
        if values.len() != result.columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "table function `{name}` row has {} values for {} columns",
                values.len(),
                result.columns.len()
            )));
        }
        out.push(values);
    }
    Ok(TableFunctionRows::materialized(columns, out))
}

pub(in crate::sql) fn registered_table_function_row_stream(
    name: &str,
    result: SQLTableFunctionStream,
    _alias: Option<&str>,
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    if result.columns.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "table function `{name}` returned no columns"
        )));
    }
    let expected_width = result.columns.len();
    let columns = result
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            column_aliases
                .get(index)
                .cloned()
                .unwrap_or_else(|| column.clone())
        })
        .collect::<Vec<_>>();
    let function_name = name.to_string();
    Ok(TableFunctionRows::new(
        columns,
        Box::new(result.rows.map(
            move |values| -> uqa_execution::ExecResult<uqa_execution::PhysicalRow> {
                let values = values.map_err(uqa_execution::ExecError::from)?;
                if values.len() != expected_width {
                    return Err(SQLError::TypeMismatch(format!(
                "table function `{function_name}` row has {} values for {expected_width} columns",
                values.len()
            ))
                    .into());
                }
                Ok(uqa_execution::PhysicalRow::from_values(values))
            },
        )),
    ))
}
