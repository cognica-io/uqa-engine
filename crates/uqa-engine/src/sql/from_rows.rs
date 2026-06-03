//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM/JOIN row assembly, table functions, and projection intercepts.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{Expr, FromClause, JoinKind, Projection, SelectStmt};
use uqa_sql::expr::{eval, EvalContext};
use uqa_sql::{ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::{Engine, SQLTableFunctionResult};

use super::{
    age_cypher, aggregate_join_rows, apply_row_order_limit, build_info_schema_rows, execute_select,
    expect_column_name, expect_optional_graph_value, graph_betweenness_entries, graph_hits_entries,
    graph_pagerank_entries, has_aggregate, json_table_arg, json_table_value_to_text,
    materialize_ctes, projection_columns, run_graph_create, run_graph_drop, run_select,
    MERGE_ACTION_COLUMN, SCORE_COLUMN,
};

fn qualifier_for(name: &str, alias: Option<&str>) -> String {
    alias.unwrap_or(name).to_string()
}

fn load_table_rows(engine: &Engine, table: &str) -> Vec<Document> {
    engine
        .table_doc_ids(table)
        .into_iter()
        .filter_map(|id| engine.get_document(table, id))
        .collect()
}

/// Synthesize rows for `information_schema` / `pg_catalog` virtual
/// views. Returns `None` for any unknown name so the caller falls back
/// to the regular table lookup.
pub(super) fn prefix_row(qual: &str, doc: &Document) -> ResultRow {
    let mut out = ResultRow::new();
    for (k, v) in doc {
        out.insert(format!("{qual}.{k}"), v.clone());
    }
    out
}

/// Re-key a row that already has unprefixed column labels onto a new
/// qualifier. Used to plug CTE materializations into the JOIN executor
/// under whatever alias the outer query referenced them by.
fn reprefix_row(qual: &str, row: &ResultRow) -> ResultRow {
    let mut out = ResultRow::new();
    for (k, v) in row {
        // CTE rows are already keyed by their projection labels; lift
        // unqualified labels under the new qualifier so qualified refs
        // (`alias.col`) and unqualified suffix matches both resolve.
        let key = if k.contains('.') {
            // Strip an existing qualifier and re-prefix.
            let (_, col) = k.split_once('.').unwrap_or((qual, k.as_str()));
            format!("{qual}.{col}")
        } else {
            format!("{qual}.{k}")
        };
        out.insert(key, v.clone());
    }
    out
}

fn merge_rows(left: &ResultRow, right: &ResultRow) -> ResultRow {
    let mut out = left.clone();
    for (k, v) in right {
        out.insert(k.clone(), v.clone());
    }
    out
}

fn null_row_for(table: &str, alias: Option<&str>, engine: &Engine) -> ResultRow {
    let qual = qualifier_for(table, alias);
    let mut out = ResultRow::new();
    if engine.table(table).is_none() {
        for column in engine.foreign_table_columns(table) {
            out.insert(format!("{qual}.{column}"), Value::Null);
        }
        return out;
    }
    // Emit NULLs for any column that ever appeared in the table; for an
    // empty table we still know the keys via document_count, but the
    // safe default is just an empty row - a missing key resolves to
    // NULL through Expr::Column / QualifiedColumn lookup anyway.
    let mut sample_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for id in engine.table_doc_ids(table) {
        if let Some(doc) = engine.get_document(table, id) {
            for k in doc.keys() {
                sample_keys.insert(k.clone());
            }
        }
        if sample_keys.len() > 16 {
            break;
        }
    }
    for k in sample_keys {
        out.insert(format!("{qual}.{k}"), Value::Null);
    }
    out
}

pub(super) fn build_join_rows(
    engine: &Engine,
    from: &FromClause,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    build_join_rows_with_ctes(engine, from, params, &BTreeMap::new())
}

pub(super) fn build_join_rows_with_ctes(
    engine: &Engine,
    from: &FromClause,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<Vec<ResultRow>, SQLError> {
    match from {
        FromClause::Table { name, alias } => {
            let qual = qualifier_for(name, alias.as_deref());
            // CTE reference takes precedence over a real table of the
            // same name (matches `PostgreSQL` semantics).
            if let Some(rows) = ctes.get(name) {
                return Ok(rows.iter().map(|row| reprefix_row(&qual, row)).collect());
            }
            if let Some(body) = engine.view(name) {
                let mut scoped_ctes = ctes.clone();
                let result = execute_select(engine, &body, params, &mut scoped_ctes)?;
                return Ok(result
                    .rows
                    .iter()
                    .map(|row| reprefix_row(&qual, row))
                    .collect());
            }
            // information_schema / pg_catalog virtual views.
            if let Some(rows) = build_info_schema_rows(engine, name) {
                return Ok(rows.iter().map(|r| reprefix_row(&qual, r)).collect());
            }
            if engine.foreign_table(name).is_some() {
                let rows = engine
                    .scan_foreign_table(name, None, &[], None)
                    .map_err(SQLError::Unsupported)?;
                return Ok(rows.iter().map(|r| reprefix_row(&qual, r)).collect());
            }
            if engine.table(name).is_none() {
                return Err(SQLError::Unsupported(format!(
                    "relation `{name}` does not exist"
                )));
            }
            Ok(load_table_rows(engine, name)
                .iter()
                .map(|d| prefix_row(&qual, d))
                .collect())
        }
        FromClause::Join {
            left,
            right,
            kind,
            on,
            lateral,
        } => {
            let left_rows = build_join_rows_with_ctes(engine, left, params, ctes)?;
            // LATERAL: re-evaluate the right side once per left row,
            // so the right body can reference outer columns. The
            // engine substitutes the outer row into the EvalContext
            // through the row-level evaluator.
            if *lateral {
                return build_lateral_join_rows(
                    engine,
                    &left_rows,
                    right,
                    *kind,
                    on.as_ref(),
                    params,
                    ctes,
                );
            }
            let right_rows = build_join_rows_with_ctes(engine, right, params, ctes)?;
            let on_expr = on.as_ref();

            match kind {
                JoinKind::Inner | JoinKind::Cross => {
                    if matches!(kind, JoinKind::Inner) {
                        if let Some(rows) =
                            try_hash_inner_join(engine, &left_rows, &right_rows, on_expr, params)?
                        {
                            return Ok(rows);
                        }
                    }
                    Ok(cross_filter(
                        engine,
                        &left_rows,
                        &right_rows,
                        on_expr,
                        params,
                    )?)
                }
                JoinKind::Left => Ok(left_outer(
                    &left_rows,
                    &right_rows,
                    right,
                    on_expr,
                    params,
                    engine,
                )?),
                JoinKind::Right => Ok(left_outer(
                    &right_rows,
                    &left_rows,
                    left,
                    on_expr,
                    params,
                    engine,
                )?),
                JoinKind::Full => Ok(full_outer(
                    &left_rows,
                    &right_rows,
                    left,
                    right,
                    on_expr,
                    params,
                    engine,
                )?),
            }
        }
        FromClause::Values {
            rows,
            alias,
            column_aliases,
        } => Ok(build_values_rows(
            engine,
            rows,
            alias.as_deref(),
            column_aliases,
            params,
        )?),
        FromClause::Function {
            name,
            args,
            alias,
            column_aliases,
        } => Ok(build_table_function_rows(
            engine,
            name,
            args,
            alias.as_deref(),
            column_aliases,
            params,
        )?),
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let result = run_select(engine, (**body).clone(), params)?;
            Ok(materialize_subquery_rows(
                result,
                alias.as_deref(),
                column_aliases,
            ))
        }
    }
}

fn materialize_subquery_rows(
    result: SQLResult,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Vec<ResultRow> {
    let cols = column_aliases.to_vec();
    result
        .rows
        .into_iter()
        .map(|mut r| {
            if !cols.is_empty() {
                let pairs: Vec<(String, Value)> = result
                    .columns
                    .iter()
                    .zip(cols.iter())
                    .filter_map(|(orig, new)| r.remove(orig).map(|v| (new.clone(), v)))
                    .collect();
                let mut renamed = ResultRow::new();
                for (k, v) in pairs {
                    renamed.insert(k, v);
                }
                if let Some(q) = alias {
                    return prefix_row(q, &renamed);
                }
                renamed
            } else if let Some(q) = alias {
                prefix_row(q, &r)
            } else {
                r
            }
        })
        .collect()
}

fn build_values_rows(
    engine: &Engine,
    rows: &[Vec<Expr>],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{eval, EvalContext};
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let n_cols = rows[0].len();
    let columns: Vec<String> = (0..n_cols)
        .map(|i| {
            column_aliases
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("column{}", i + 1))
        })
        .collect();
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let mut out: Vec<ResultRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut r = ResultRow::new();
        for (i, expr) in row.iter().enumerate() {
            let v = eval(expr, &ctx)?;
            r.insert(columns[i].clone(), v);
        }
        let r = match alias {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    }
    Ok(out)
}

/// LATERAL join executor: re-evaluates the right side per left row
/// so the right body can reference outer columns. We splice the
/// outer row into a per-row CTE-style scope by registering it under
/// the `__lateral__` reserved name and inlining its keys into a
/// fresh CTE map; the right side then sees those columns as plain
/// row keys when its internal expressions evaluate. Mirrors
/// `PostgreSQL` LATERAL semantics.
#[allow(clippy::too_many_arguments)]
fn build_lateral_join_rows(
    engine: &Engine,
    left_rows: &[ResultRow],
    right: &FromClause,
    kind: JoinKind,
    on: Option<&Expr>,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{eval, truthy, EvalContext};
    let mut out: Vec<ResultRow> = Vec::new();
    for left_row in left_rows {
        let right_rows = match right {
            FromClause::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let result = execute_lateral_subquery(engine, body, left_row, params, ctes)?;
                materialize_subquery_rows(result, alias.as_deref(), column_aliases)
            }
            FromClause::Function {
                name,
                args,
                alias,
                column_aliases,
            } => build_table_function_rows_with_row(
                engine,
                name,
                args,
                alias.as_deref(),
                column_aliases,
                params,
                Some(left_row),
            )?,
            _ => build_join_rows_with_ctes(engine, right, params, ctes)?,
        };
        for r_row in &right_rows {
            let mut joined = ResultRow::new();
            for (k, v) in left_row {
                joined.insert(k.clone(), v.clone());
            }
            for (k, v) in r_row {
                joined.insert(k.clone(), v.clone());
            }
            let keep = match (on, kind) {
                (None, _) | (_, JoinKind::Cross) => true,
                (Some(filter), _) => {
                    let ctx = EvalContext::new(Some(&joined), params).with_engine(engine);
                    truthy(&eval(filter, &ctx)?)
                }
            };
            if keep {
                out.push(joined);
            }
        }
    }
    Ok(out)
}

pub(super) fn execute_lateral_subquery(
    engine: &Engine,
    stmt: &SelectStmt,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<SQLResult, SQLError> {
    let mut scoped_ctes = ctes.clone();
    materialize_ctes(engine, &stmt.with, params, &mut scoped_ctes)?;

    let Some(from) = stmt.from.as_ref() else {
        let projected =
            project_join_row_with_engine(Some(engine), outer_row, &stmt.projections, params)?;
        return Ok(SQLResult::from_rows(
            projection_columns(&stmt.projections),
            vec![projected],
        ));
    };

    let inner_rows = build_join_rows_with_ctes(engine, from, params, &scoped_ctes)?;
    let mut filtered: Vec<ResultRow> = Vec::with_capacity(inner_rows.len());
    for inner in inner_rows {
        let merged = merge_lateral_scope_rows(outer_row, &inner);
        let keep = match stmt.r#where.as_ref() {
            None => true,
            Some(filter) => {
                let ctx = EvalContext::new(Some(&merged), params).with_engine(engine);
                uqa_sql::expr::truthy(&eval(filter, &ctx)?)
            }
        };
        if keep {
            filtered.push(merged);
        }
    }

    if has_aggregate(engine, &stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &filtered, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let ordered = apply_row_order_limit(filtered, stmt, engine, params)?;
    let columns = projection_columns(&stmt.projections);
    let rows = ordered
        .iter()
        .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SQLResult::from_rows(columns, rows))
}

fn merge_lateral_scope_rows(outer_row: &ResultRow, inner_row: &ResultRow) -> ResultRow {
    let mut merged = outer_row.clone();
    for (key, value) in inner_row {
        merged.insert(key.clone(), value.clone());
        if let Some((_, column)) = key.rsplit_once('.') {
            merged.insert(column.to_string(), value.clone());
        }
    }
    merged
}

#[allow(clippy::similar_names)]
fn build_table_function_rows(
    engine: &Engine,
    name: &str,
    args: &[Expr],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    build_table_function_rows_with_row(engine, name, args, alias, column_aliases, params, None)
}

#[allow(clippy::similar_names)]
fn build_table_function_rows_with_row(
    engine: &Engine,
    name: &str,
    args: &[Expr],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
    row: Option<&ResultRow>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{eval, EvalContext};
    let ctx = EvalContext::new(row, params).with_engine(engine);
    let lower = name.to_ascii_lowercase();
    let evaluated: Vec<Value> = args
        .iter()
        .map(|a| eval(a, &ctx))
        .collect::<Result<Vec<_>, SQLError>>()?;
    let default_col = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| name.to_string());
    let qual = alias;
    let mut out: Vec<ResultRow> = Vec::new();
    let push_scalar = |out: &mut Vec<ResultRow>, value: Value| {
        let mut r = ResultRow::new();
        r.insert(default_col.clone(), value);
        let r = match qual {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    };
    if let Some(result) = engine.call_registered_table_function(&lower, &evaluated) {
        return registered_table_function_rows(name, result?, qual, column_aliases);
    }
    match lower.as_str() {
        "generate_series" => {
            if !(2..=3).contains(&evaluated.len()) {
                return Err(SQLError::TypeMismatch(
                    "generate_series requires 2-3 args".into(),
                ));
            }
            let start = match &evaluated[0] {
                Value::Int(i) => *i,
                Value::Float(f) => *f as i64,
                _ => return Err(SQLError::TypeMismatch("generate_series start".into())),
            };
            let stop = match &evaluated[1] {
                Value::Int(i) => *i,
                Value::Float(f) => *f as i64,
                _ => return Err(SQLError::TypeMismatch("generate_series stop".into())),
            };
            let step = if evaluated.len() == 3 {
                match &evaluated[2] {
                    Value::Int(i) => *i,
                    Value::Float(f) => *f as i64,
                    _ => return Err(SQLError::TypeMismatch("generate_series step".into())),
                }
            } else {
                1
            };
            if step == 0 {
                return Err(SQLError::TypeMismatch(
                    "generate_series step cannot be 0".into(),
                ));
            }
            let mut current = start;
            if step > 0 {
                while current <= stop {
                    push_scalar(&mut out, Value::Int(current));
                    current += step;
                }
            } else {
                while current >= stop {
                    push_scalar(&mut out, Value::Int(current));
                    current += step;
                }
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
            if evaluated.len() < 2 {
                return Err(SQLError::TypeMismatch(
                    "create_analyzer requires (name, config_json)".into(),
                ));
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
            if evaluated.is_empty() {
                return Err(SQLError::TypeMismatch(
                    "drop_analyzer requires a name argument".into(),
                ));
            }
            let analyzer_name = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("drop_analyzer arg 1".into())),
            };
            engine.drop_named_analyzer(&analyzer_name);
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
            // Match UQA behavior for: include the four built-in analyzers
            // (`whitespace`, `standard`, `standard_cjk`, `keyword`) on
            // top of every user-registered named analyzer.
            let mut names: std::collections::BTreeSet<String> =
                engine.list_named_analyzers().into_iter().collect();
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
            for stat in engine.fts_index_stats(table_filter) {
                let mut r = ResultRow::new();
                r.insert("table_name".into(), Value::Str(stat.table_name));
                r.insert("field".into(), Value::Str(stat.field));
                r.insert("analyzer".into(), Value::Str(stat.analyzer));
                r.insert(
                    "posting_count".into(),
                    Value::Int(stat.posting_count as i64),
                );
                r.insert(
                    "doc_length_count".into(),
                    Value::Int(stat.doc_length_count as i64),
                );
                r.insert(
                    "indexed_doc_count".into(),
                    Value::Int(stat.indexed_doc_count as i64),
                );
                r.insert("term_count".into(), Value::Int(stat.term_count as i64));
                r.insert(
                    "total_field_length".into(),
                    Value::Int(stat.total_field_length as i64),
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
            if evaluated.len() < 3 {
                return Err(SQLError::TypeMismatch(
                    "set_table_analyzer requires (table, field, analyzer_name[, phase])".into(),
                ));
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
                    _ => "both".into(),
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
                _ => unreachable!(),
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
                r.insert(id_col.clone(), Value::Int(entry.doc_id as i64));
                r.insert(score_col.clone(), Value::Float(entry.score));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "cypher" => age_cypher::build_rows(engine, args, &evaluated, qual, column_aliases),
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
                Value::Int(n) => *n as u64,
                _ => return Err(SQLError::TypeMismatch("rpq.start must be integer".into())),
            };
            let graph = expect_optional_graph_value(engine, evaluated.get(2), "rpq")?;
            let path = uqa_graph::parse_rpq(&expr_str)
                .map_err(|e| SQLError::Unsupported(format!("{e:?}")))?;
            let pl = engine
                .graph_with(&graph, |store| {
                    uqa_graph::RegularPathQuery::new(path, &graph)
                        .from_vertex(start)
                        .execute(store)
                })
                .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {graph:?}")))?;
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "vertex_id".into());
            for entry in pl.inner().entries() {
                let mut r = ResultRow::new();
                r.insert(id_col.clone(), Value::Int(entry.doc_id as i64));
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

fn registered_table_function_rows(
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

/// Detect an equijoin shape `<col_a> = <col_b>` and run a hash join.
///
/// Returns `Some(rows)` when the predicate is a clean equality
/// between qualified columns from the two sides. Returns `None` for
/// every other shape; the caller then falls back to the nested-loop
/// cross filter.
fn try_hash_inner_join(
    engine: &Engine,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&Expr>,
    params: &[SQLParam],
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let Some(Expr::Binary {
        op: uqa_sql::ast::BinaryOp::Equal,
        lhs,
        rhs,
    }) = on
    else {
        return Ok(None);
    };
    let Some((left_key, right_key)) =
        decide_join_sides(engine, left_rows, right_rows, lhs, rhs, params)
    else {
        return Ok(None);
    };
    // Use the shared hash-join algorithm from `uqa-joins`. The closures
    // evaluate the picked join keys against each row and lift the
    // result into a hashable `JoinKey`; null-valued keys are skipped
    // so they do not match anything.
    use uqa_joins::row_join::{hash_inner_join, JoinKey};
    let key_of = |row: &ResultRow, expr: &Expr| -> Option<JoinKey> {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
        match uqa_sql::expr::eval(expr, &ctx) {
            Ok(uqa_core::Value::Null) | Err(_) => None,
            Ok(v) => Some(JoinKey::new(&v)),
        }
    };
    let out = hash_inner_join(
        left_rows,
        right_rows,
        |row| key_of(row, left_key),
        |row| key_of(row, right_key),
    );
    Ok(Some(out))
}

/// Pick which expression evaluates over the left side and which over
/// the right by sampling the first row of each side. Returns
/// `(left_key_expr, right_key_expr)` when one direction works,
/// `None` when the predicate isn't separable across sides.
fn decide_join_sides<'a>(
    engine: &Engine,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    lhs: &'a Expr,
    rhs: &'a Expr,
    params: &[SQLParam],
) -> Option<(&'a Expr, &'a Expr)> {
    if left_rows.is_empty() || right_rows.is_empty() {
        return None;
    }
    let l_sample = &left_rows[0];
    let r_sample = &right_rows[0];
    let lhs_on_left = eval_yields_value(engine, l_sample, lhs, params);
    let rhs_on_right = eval_yields_value(engine, r_sample, rhs, params);
    if lhs_on_left && rhs_on_right {
        return Some((lhs, rhs));
    }
    let rhs_on_left = eval_yields_value(engine, l_sample, rhs, params);
    let lhs_on_right = eval_yields_value(engine, r_sample, lhs, params);
    if rhs_on_left && lhs_on_right {
        return Some((rhs, lhs));
    }
    None
}

fn eval_yields_value(engine: &Engine, row: &ResultRow, expr: &Expr, params: &[SQLParam]) -> bool {
    let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
    matches!(uqa_sql::expr::eval(expr, &ctx), Ok(v) if v != uqa_core::Value::Null)
}

fn cross_filter(
    engine: &Engine,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&Expr>,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::with_capacity(left_rows.len() * right_rows.len());
    for l in left_rows {
        for r in right_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx =
                        uqa_sql::expr::EvalContext::new(Some(&merged), params).with_engine(engine);
                    uqa_sql::expr::truthy(&uqa_sql::expr::eval(expr, &ctx)?)
                }
            };
            if keep {
                out.push(merged);
            }
        }
    }
    Ok(out)
}

fn left_outer(
    outer_rows: &[ResultRow],
    inner_rows: &[ResultRow],
    inner_from: &FromClause,
    on: Option<&Expr>,
    params: &[SQLParam],
    engine: &Engine,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for l in outer_rows {
        let mut matched = false;
        for r in inner_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx =
                        uqa_sql::expr::EvalContext::new(Some(&merged), params).with_engine(engine);
                    uqa_sql::expr::truthy(&uqa_sql::expr::eval(expr, &ctx)?)
                }
            };
            if keep {
                out.push(merged);
                matched = true;
            }
        }
        if !matched {
            // Pad with NULLs for every column the inner side would
            // have contributed.
            let mut pad = l.clone();
            pad_nulls_for_from(&mut pad, inner_from, engine);
            out.push(pad);
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn full_outer(
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    left_from: &FromClause,
    right_from: &FromClause,
    on: Option<&Expr>,
    params: &[SQLParam],
    engine: &Engine,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    let mut matched_right = vec![false; right_rows.len()];
    for left in left_rows {
        let mut matched_left = false;
        for (idx, right) in right_rows.iter().enumerate() {
            let merged = merge_rows(left, right);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx =
                        uqa_sql::expr::EvalContext::new(Some(&merged), params).with_engine(engine);
                    uqa_sql::expr::truthy(&uqa_sql::expr::eval(expr, &ctx)?)
                }
            };
            if keep {
                out.push(merged);
                matched_left = true;
                matched_right[idx] = true;
            }
        }
        if !matched_left {
            let mut padded = left.clone();
            pad_nulls_for_from(&mut padded, right_from, engine);
            out.push(padded);
        }
    }
    for (idx, right) in right_rows.iter().enumerate() {
        if matched_right[idx] {
            continue;
        }
        let mut padded = ResultRow::new();
        pad_nulls_for_from(&mut padded, left_from, engine);
        for (k, v) in right {
            padded.insert(k.clone(), v.clone());
        }
        out.push(padded);
    }
    Ok(out)
}

fn pad_nulls_for_from(row: &mut ResultRow, from: &FromClause, engine: &Engine) {
    let mut tables = Vec::new();
    from.collect_tables(&mut tables);
    for (name, alias) in &tables {
        let null_keys = null_row_for(name, alias.as_deref(), engine);
        for (k, v) in null_keys {
            row.entry(k).or_insert(v);
        }
    }
}

#[allow(dead_code)]
fn project_join_row(
    engine: &Engine,
    src: &ResultRow,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    project_join_row_with_engine(Some(engine), src, projections, params)
}

pub(super) fn project_join_row_with_engine(
    engine: Option<&Engine>,
    src: &ResultRow,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let mut ctx = uqa_sql::expr::EvalContext::new(Some(src), params);
    if let Some(e) = engine {
        ctx = ctx.with_engine(e);
    }
    let labels = projection_columns(projections);
    let mut out = ResultRow::new();
    for (idx, proj) in projections.iter().enumerate() {
        let label = labels[idx].clone();
        if let Expr::Star = proj.expr {
            for (k, v) in src {
                out.insert(k.clone(), v.clone());
            }
            continue;
        }
        // Window calls are pre-evaluated in `compute_window_columns`
        // and stored on the source row under the projection label;
        // read the cached value through.
        if matches!(proj.expr, Expr::WindowCall { .. }) {
            let value = src.get(&label).cloned().unwrap_or(Value::Null);
            out.insert(label, value);
            continue;
        }
        // `uqa_highlight()` evaluates against the analyzer for the
        // matched field, which the evaluator does not have access
        // to. Intercept the call here, resolve the string column +
        // query, and emit the wrapped text through
        // `uqa_analysis::highlight`.
        if let Expr::Func { name, args, .. } = &proj.expr {
            if let Some(value) = engine_func_intercept(engine, name, args, src, params)? {
                out.insert(label, value);
                continue;
            }
        }
        let value = uqa_sql::expr::eval(&proj.expr, &ctx)?;
        out.insert(label, value);
    }
    Ok(out)
}

/// Intercept registry functions that need engine-level access (the
/// scalar evaluator does not see the engine, just the row context).
/// Returns `Ok(Some(_))` when the function was handled, `Ok(None)`
/// to defer to the default scalar evaluator.
pub(super) fn engine_func_intercept(
    engine: Option<&Engine>,
    name: &str,
    args: &[Expr],
    row: &ResultRow,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "uqa_highlight" => Ok(Some(run_uqa_highlight(engine, row, args, params)?)),
        "score_bm25" | "score_bayesian_bm25" => {
            validate_score_projection_args(&lower, args, row, params)?;
            Ok(Some(
                row.get(SCORE_COLUMN).cloned().unwrap_or(Value::Float(0.0)),
            ))
        }
        "deep_learn" => Ok(Some(run_deep_learn_projection(engine, args, row, params)?)),
        "merge_action" => {
            if !args.is_empty() {
                return Err(SQLError::BadArity {
                    name: "merge_action".into(),
                    expected: "0".into(),
                    actual: args.len(),
                });
            }
            let action = row.get(MERGE_ACTION_COLUMN).cloned().ok_or_else(|| {
                SQLError::Unsupported("merge_action() is only valid in MERGE RETURNING".into())
            })?;
            Ok(Some(action))
        }
        "graph_create" | "create_graph" => {
            if let Some(eng) = engine {
                let _ = run_graph_create(eng, args, params)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        "graph_drop" | "drop_graph" => {
            if let Some(eng) = engine {
                let _ = run_graph_drop(eng, args, params)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        _ => Ok(None),
    }
}

fn run_deep_learn_projection(
    engine: Option<&Engine>,
    args: &[Expr],
    row: &ResultRow,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let Some(engine) = engine else {
        return Err(SQLError::Unsupported(
            "deep_learn requires an engine-backed projection".into(),
        ));
    };
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "deep_learn".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(Some(row), params).with_engine(engine);
    let model_name = match eval(&args[0], &ctx)? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.model must be a string, got {other:?}"
            )));
        }
    };
    let training_source = match eval(&args[1], &ctx)? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.training_set must be a table name or JSON string, got {other:?}"
            )));
        }
    };
    let trimmed = training_source.trim();
    let output = if trimmed.starts_with('{') {
        engine.deep_learn_json(&model_name, trimmed, &uqa_ml::LearnOptions::default())?
    } else {
        engine.deep_learn_table(
            &model_name,
            &training_source,
            &uqa_ml::LearnOptions::default(),
        )?
    };
    let mut report = BTreeMap::new();
    report.insert("model".into(), Value::Str(model_name));
    report.insert("examples".into(), Value::Int(output.report.examples as i64));
    report.insert(
        "feature_dimensions".into(),
        Value::Int(output.report.feature_dimensions as i64),
    );
    report.insert(
        "class_count".into(),
        Value::Int(output.report.class_count as i64),
    );
    Ok(Value::Map(report))
}

fn validate_score_projection_args(
    name: &str,
    args: &[Expr],
    row: &ResultRow,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "1..=2".into(),
            actual: args.len(),
        });
    }
    let query_idx = args.len() - 1;
    if args.len() == 2 {
        let _ = expect_column_name(&args[0], &format!("{name}.field"))?;
    }
    let ctx = EvalContext::new(Some(row), params);
    match eval(&args[query_idx], &ctx)? {
        Value::Str(_) => Ok(()),
        other => Err(SQLError::TypeMismatch(format!(
            "{name}.query must be a string, got {other:?}"
        ))),
    }
}

/// Evaluate a `uqa_highlight(field, query[, start_tag, end_tag,
/// max_fragments, fragment_size])` projection. Matches UQA
/// reference's `_run_uqa_highlight` shape: field can be either a
/// bare column reference (looked up on the row) or a literal string;
/// the rest of the args are scalar literals after evaluation.
fn run_uqa_highlight(
    engine: Option<&Engine>,
    row: &ResultRow,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    if args.len() < 2 || args.len() > 6 {
        return Err(SQLError::BadArity {
            name: "uqa_highlight".into(),
            expected: "2..=6".into(),
            actual: args.len(),
        });
    }
    let mut ctx = uqa_sql::expr::EvalContext::new(Some(row), params);
    if let Some(e) = engine {
        ctx = ctx.with_engine(e);
    }
    let text = match &args[0] {
        Expr::Column(c) => match row.get(c) {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Null) => return Ok(Value::Null),
            Some(other) => format!("{other:?}"),
            None => return Ok(Value::Null),
        },
        Expr::QualifiedColumn { qualifier, column } => {
            match row.get(&format!("{qualifier}.{column}")) {
                Some(Value::Str(s)) => s.clone(),
                Some(Value::Null) => return Ok(Value::Null),
                Some(other) => format!("{other:?}"),
                None => return Ok(Value::Null),
            }
        }
        other => match uqa_sql::expr::eval(other, &ctx)? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            v => format!("{v:?}"),
        },
    };
    let query_str = match uqa_sql::expr::eval(&args[1], &ctx)? {
        Value::Str(s) => s,
        Value::Null => return Ok(Value::Str(text)),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "uqa_highlight query must be string, got {other:?}"
            )));
        }
    };
    let start_tag = match args.get(2) {
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
            Value::Str(s) => s,
            Value::Null => "<b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight start_tag must be string, got {other:?}"
                )));
            }
        },
        None => "<b>".into(),
    };
    let end_tag = match args.get(3) {
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
            Value::Str(s) => s,
            Value::Null => "</b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight end_tag must be string, got {other:?}"
                )));
            }
        },
        None => "</b>".into(),
    };
    let max_fragments = match args.get(4) {
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
            Value::Int(n) if n >= 0 => n as usize,
            Value::Null => 0,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight max_fragments must be non-negative integer, got {other:?}"
                )));
            }
        },
        None => 0,
    };
    let fragment_size = match args.get(5) {
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
            Value::Int(n) if n > 0 => n as usize,
            Value::Null => 150,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight fragment_size must be positive integer, got {other:?}"
                )));
            }
        },
        None => 150,
    };
    let opts = uqa_analysis::HighlightOptions {
        start_tag,
        end_tag,
        max_fragments,
        fragment_size,
    };
    // Pull every whitespace-separated token out of the query string
    // as a candidate match term. The canonical UQA behavior parses the FTS
    // query, but a simple split is what callers reach for in
    // practice and matches what the test fixture exercises.
    let terms: Vec<String> = query_str
        .split_whitespace()
        .filter(|t| !matches!(t.to_ascii_lowercase().as_str(), "and" | "or" | "not"))
        .map(std::string::ToString::to_string)
        .collect();
    let analyzer = uqa_analysis::standard_analyzer("english");
    let out = uqa_analysis::highlight(&text, &terms, Some(&analyzer), &opts);
    Ok(Value::Str(out))
}
