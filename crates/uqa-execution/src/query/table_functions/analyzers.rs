//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyzer command scheduling and full-text index result materialization.

use super::{context::AnalyzerTableFunctions, TableFunctionRows};
use uqa_core::Value;
use uqa_sql::{
    semantics::{
        source_filters::checked_integer_value,
        table_function_arguments::{
            create_analyzer_arguments, drop_analyzer_arguments, fts_index_stats_table,
            require_no_arguments, set_table_analyzer_arguments,
        },
    },
    SQLError,
};

pub(super) fn build_rows(
    runtime: &dyn AnalyzerTableFunctions,
    lower: &str,
    evaluated: &[Value],
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    let mut out = Vec::new();
    match lower {
        "create_analyzer" => {
            let (analyzer_name, config_json) = create_analyzer_arguments(evaluated)?;
            runtime
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
            let analyzer_name = drop_analyzer_arguments(evaluated)?;
            let removed = runtime
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
            require_no_arguments("list_analyzers", evaluated)?;
            let mut names: std::collections::BTreeSet<String> = runtime
                .list_named_analyzers()
                .map_err(SQLError::Unsupported)?
                .into_iter()
                .collect();
            for builtin in uqa_analysis::builtin_analyzer_names() {
                names.insert(builtin);
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
        "analyze_text" => {
            let (name, input) =
                uqa_sql::semantics::table_function_arguments::analyze_text_arguments(evaluated)?;
            let diagnostic = runtime
                .analyze_text(&name, &input)
                .map_err(SQLError::Unsupported)?;
            let column = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "analysis".into());
            Ok(TableFunctionRows::materialized(
                vec![column],
                vec![vec![diagnostic]],
            ))
        }
        "fts_index_stats" => index_stat_rows(runtime, evaluated),
        "set_table_analyzer" => {
            let (target_table, field, analyzer_name, phase) =
                set_table_analyzer_arguments(evaluated)?;
            runtime
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
        _ => unreachable!("analyzer table function selected by the caller"),
    }
}

fn index_stat_rows(
    runtime: &dyn AnalyzerTableFunctions,
    evaluated: &[Value],
) -> Result<TableFunctionRows, SQLError> {
    let mut out = Vec::new();
    let table_filter = fts_index_stats_table(evaluated)?;
    for stat in runtime.fts_index_stats(table_filter)? {
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
