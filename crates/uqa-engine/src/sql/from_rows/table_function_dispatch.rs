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
    prefix_row, PlanSubqueryArena, ResultRow, SQLError, SQLTableFunctionResult,
    SQLTableFunctionStream, ScalarEvalContext, ScalarExpr, TableFunctionEvalContext, Value,
};

#[allow(clippy::similar_names)]
pub(in crate::sql) fn build_table_function_rows_with_row(
    context: &TableFunctionEvalContext<'_>,
    name: &str,
    args: &[ScalarExpr],
    alias: Option<&str>,
    column_aliases: &[String],
    column_types: &[String],
    row: Option<&ResultRow>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::unknown_function_error;
    let engine = context.engine;
    let subquery_arena = PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
    let ctx = ScalarEvalContext::new(row, context.params)
        .with_function_hook(context.eval_hook)
        .with_subquery_runner(&subquery_arena);
    let identity = name.to_ascii_lowercase();
    let lower = crate::sql::builtin_function_dispatch_name(&identity);
    let call_args = eval_call_arguments(args, &ctx)?;
    let has_named_args = call_args.iter().any(|(name, _)| name.is_some());
    let evaluated: Vec<Value> = call_args.iter().map(|(_, value)| value.clone()).collect();
    let default_col = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| alias.unwrap_or(name).to_string());
    let qual = alias;
    let mut out: Vec<ResultRow> = Vec::new();
    let push_scalar = |out: &mut Vec<ResultRow>, value: Value| {
        let mut r = ResultRow::new();
        r.insert(default_col.clone(), value.clone());
        if column_aliases.is_empty() && alias.is_some() && default_col != name {
            r.insert(name.to_string(), value);
        }
        let r = match qual {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    };
    if !has_named_args {
        if let Some(result) = engine.call_registered_table_function(&identity, &evaluated) {
            return registered_table_function_rows(name, result?, qual, column_aliases);
        }
    }
    if let Some(result) =
        crate::sql::plpgsql_exec::call_user_table_function(engine, &identity, &call_args)
    {
        return registered_table_function_rows(name, result?, qual, column_aliases);
    }
    if has_named_args {
        return Err(unknown_function_error(&lower, &call_args));
    }
    match lower.as_str() {
        "generate_series" => {
            for value in generate_series_values(evaluated)? {
                push_scalar(&mut out, value);
            }
            Ok(out)
        }
        "unnest" => {
            for value in &evaluated {
                if let Value::List(items) = value {
                    for item in items {
                        push_scalar(&mut out, item.clone());
                    }
                } else {
                    push_scalar(&mut out, value.clone());
                }
            }
            Ok(out)
        }
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
            Ok(out)
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
                let mut r = ResultRow::new();
                r.insert(key_col.clone(), Value::Str(k));
                r.insert(val_col.clone(), json_table_value_to_text(&v));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
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
                let mut r = ResultRow::new();
                r.insert(col.clone(), json_table_value_to_text(&v));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        // -------------------------------------------------------------
        // Analyzer DDL exposed as table-functions. Mirror the canonical UQA implementation's
        // _build_create_analyzer / _build_drop_analyzer /
        // _build_list_analyzers / _build_set_table_analyzer.
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
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "create_analyzer".into()),
                Value::Str(format!("analyzer '{analyzer_name}' created")),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
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
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "drop_analyzer".into()),
                Value::Str(format!("analyzer '{analyzer_name}' dropped")),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "list_analyzers" => {
            if !evaluated.is_empty() {
                return Err(SQLError::BadArity {
                    name: "list_analyzers".into(),
                    expected: "0".into(),
                    actual: evaluated.len(),
                });
            }
            // Match UQA behavior for: include the four built-in analyzers
            // (`whitespace`, `standard`, `standard_cjk`, `keyword`) on
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
                let mut r = ResultRow::new();
                r.insert(key.clone(), Value::Str(n));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
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
                let mut r = ResultRow::new();
                r.insert("table_name".into(), Value::Str(stat.table_name));
                r.insert("field".into(), Value::Str(stat.field));
                r.insert("analyzer".into(), Value::Str(stat.analyzer));
                r.insert(
                    "posting_count".into(),
                    checked_integer_value(stat.posting_count, "posting count")?,
                );
                r.insert(
                    "doc_length_count".into(),
                    checked_integer_value(stat.doc_length_count, "document-length count")?,
                );
                r.insert(
                    "indexed_doc_count".into(),
                    checked_integer_value(stat.indexed_doc_count, "indexed-document count")?,
                );
                r.insert(
                    "term_count".into(),
                    checked_integer_value(stat.term_count, "term count")?,
                );
                r.insert(
                    "total_field_length".into(),
                    checked_integer_value(stat.total_field_length, "total field length")?,
                );
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
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
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "set_table_analyzer".into()),
                Value::Str(msg),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
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
                let mut r = ResultRow::new();
                r.insert(id_col.clone(), doc_id_value(entry.doc_id)?);
                r.insert(score_col.clone(), Value::Float(entry.score));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "cypher" => {
            age_cypher::build_rows(engine, args, &evaluated, qual, column_aliases, column_types)
        }
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
                let mut r = ResultRow::new();
                r.insert(id_col.clone(), doc_id_value(entry.doc_id)?);
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        other => Err(SQLError::Unsupported(format!(
            "table function `{other}` in FROM"
        ))),
    }
}

pub(in crate::sql) fn registered_table_function_rows(
    name: &str,
    result: SQLTableFunctionResult,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<Vec<ResultRow>, SQLError> {
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
        let mut row = ResultRow::new();
        for (column, value) in columns.iter().zip(values) {
            row.insert(column.clone(), value);
        }
        let row = match alias {
            Some(alias) => prefix_row(alias, &row),
            None => row,
        };
        out.push(row);
    }
    Ok(out)
}

pub(in crate::sql) fn registered_table_function_row_stream(
    name: &str,
    result: SQLTableFunctionStream,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<uqa_execution::ProjectRows, SQLError> {
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
    let qualifier = alias.map(str::to_string);
    Ok(Box::new(result.rows.map(
        move |values| -> uqa_execution::ExecResult<ResultRow> {
            let values = values.map_err(uqa_execution::ExecError::from)?;
            if values.len() != expected_width {
                return Err(SQLError::TypeMismatch(format!(
                "table function `{function_name}` row has {} values for {expected_width} columns",
                values.len()
            ))
                .into());
            }
            let mut row = ResultRow::new();
            for (column, value) in columns.iter().zip(values) {
                row.insert(column.clone(), value);
            }
            Ok(qualifier
                .as_deref()
                .map_or(row.clone(), |qualifier| prefix_row(qualifier, &row)))
        },
    )))
}
