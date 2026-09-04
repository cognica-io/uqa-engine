//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tabular and expanded query-result rendering.

use super::{ColumnType, PathBuf, SQLResult, Value, Write, HISTORY_FILE};
use uqa_sql::expr::EngineHook;

/// Render the result one column per line per row -- mirrors
/// `PostgreSQL` `psql`'s `\x` expanded display mode.
pub(super) fn print_result_expanded_with_engine(
    result: &SQLResult,
    engine: &dyn EngineHook,
    out: &mut (impl Write + ?Sized),
) {
    print_result_expanded_impl(result, Some(engine), out);
}

fn print_result_expanded_impl(
    result: &SQLResult,
    engine: Option<&dyn EngineHook>,
    out: &mut (impl Write + ?Sized),
) {
    if result.rows.is_empty() && result.columns.is_empty() {
        if result.affected_rows > 0 {
            let _ = writeln!(out, "{} row(s) affected", result.affected_rows);
        }
        return;
    }
    let columns: Vec<String> = if result.columns.is_empty() {
        let mut keys: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
        for row in &result.rows {
            for k in row.keys() {
                keys.insert(k);
            }
        }
        keys.into_iter().cloned().collect()
    } else {
        result.columns.clone()
    };
    let label_width = columns.iter().map(String::len).max().unwrap_or(0);
    for (idx, _) in result.rows.iter().enumerate() {
        let _ = writeln!(out, "-[ RECORD {} ]-", idx + 1);
        for (column, col) in columns.iter().enumerate() {
            let value = value_to_display_typed(
                result.value_at(idx, column),
                result.column_types.get(column).and_then(Option::as_ref),
                engine,
            );
            let _ = writeln!(out, "{col:<label_width$} | {value}");
        }
    }
    let _ = writeln!(out, "({} row(s))", result.rows.len());
}

pub(super) fn print_result(result: &SQLResult, out: &mut (impl Write + ?Sized)) {
    print_result_impl(result, None, out);
}

pub(super) fn print_result_with_engine(
    result: &SQLResult,
    engine: &dyn EngineHook,
    out: &mut (impl Write + ?Sized),
) {
    print_result_impl(result, Some(engine), out);
}

fn print_result_impl(
    result: &SQLResult,
    engine: Option<&dyn EngineHook>,
    out: &mut (impl Write + ?Sized),
) {
    if result.rows.is_empty() && result.columns.is_empty() {
        if result.affected_rows > 0 {
            let _ = writeln!(out, "{} row(s) affected", result.affected_rows);
        }
        return;
    }
    let columns: Vec<String> = if result.columns.is_empty() {
        let mut keys: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
        for row in &result.rows {
            for k in row.keys() {
                keys.insert(k);
            }
        }
        keys.into_iter().cloned().collect()
    } else {
        result.columns.clone()
    };

    // Compute column widths for fixed-width pretty-printing.
    let mut widths: Vec<usize> = columns.iter().map(String::len).collect();
    let stringified_rows: Vec<Vec<String>> = result
        .rows
        .iter()
        .enumerate()
        .map(|(row_index, _)| {
            columns
                .iter()
                .enumerate()
                .map(|(column_index, _)| {
                    value_to_display_typed(
                        result.value_at(row_index, column_index),
                        result
                            .column_types
                            .get(column_index)
                            .and_then(Option::as_ref),
                        engine,
                    )
                })
                .collect()
        })
        .collect();
    for row in &stringified_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    // Header.
    write_row(out, &columns, &widths);
    let separator: String = widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("-+-");
    let _ = writeln!(out, "{separator}");
    for row in &stringified_rows {
        write_row(out, row, &widths);
    }
    let _ = writeln!(out, "({} row(s))", result.rows.len());
}

/// Emit rows in `PostgreSQL` `COPY TO STDOUT` text format.
#[cfg(test)]
fn print_result_copy_text(result: &SQLResult, out: &mut (impl Write + ?Sized)) {
    print_result_copy_text_impl(result, None, out);
}

pub(super) fn print_result_copy_text_with_engine(
    result: &SQLResult,
    engine: &dyn EngineHook,
    out: &mut (impl Write + ?Sized),
) {
    print_result_copy_text_impl(result, Some(engine), out);
}

fn print_result_copy_text_impl(
    result: &SQLResult,
    engine: Option<&dyn EngineHook>,
    out: &mut (impl Write + ?Sized),
) {
    let columns: Vec<String> = if result.columns.is_empty() {
        let mut keys: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
        for row in &result.rows {
            keys.extend(row.keys());
        }
        keys.into_iter().cloned().collect()
    } else {
        result.columns.clone()
    };
    for row_index in 0..result.rows.len() {
        let cells = columns
            .iter()
            .enumerate()
            .map(|(column_index, _)| {
                copy_text_cell_typed(
                    result.value_at(row_index, column_index),
                    result
                        .column_types
                        .get(column_index)
                        .and_then(Option::as_ref),
                    engine,
                )
            })
            .collect::<Vec<_>>();
        let _ = writeln!(out, "{}", cells.join("\t"));
    }
}

#[cfg(test)]
fn copy_text_cell(value: Option<&Value>) -> String {
    copy_text_cell_typed(value, None, None)
}

fn copy_text_cell_typed(
    value: Option<&Value>,
    ty: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
) -> String {
    let Some(value) = value.filter(|value| !matches!(value, Value::Null)) else {
        return "\\N".to_string();
    };
    let text = match value {
        Value::Bool(true) => "t".to_string(),
        Value::Bool(false) => "f".to_string(),
        other => value_to_display_typed(Some(other), ty, engine),
    };
    let mut escaped = String::new();
    for character in text.chars() {
        match character {
            '\u{0008}' => escaped.push_str("\\b"),
            '\u{000c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{000b}' => escaped.push_str("\\v"),
            '\\' => escaped.push_str("\\\\"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn value_to_display_typed(
    value: Option<&Value>,
    ty: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
) -> String {
    if let Some(text) = value
        .zip(ty)
        .and_then(|(value, ty)| uqa_sql::expr::format_regtype_value(value, ty, engine).ok())
        .flatten()
    {
        return text;
    }
    if matches!(ty, Some(ColumnType::Int2Vector | ColumnType::OidVector)) {
        if let Some(value) = value.and_then(uqa_sql::expr::vector_value_to_string) {
            return value;
        }
    }
    value_to_display(value)
}

pub(super) fn write_row(out: &mut (impl Write + ?Sized), cells: &[String], widths: &[usize]) {
    let line: Vec<String> = cells
        .iter()
        .zip(widths.iter())
        .map(|(c, w)| format!("{c:width$}", c = c, width = *w))
        .collect();
    let _ = writeln!(out, "{}", line.join(" | "));
}

pub(super) fn history_path() -> Option<PathBuf> {
    // Honour the Rust test override if present; otherwise mirror
    // legacy `usql` ~/.cognica/uqa/.usql_history default.
    if let Ok(custom) = std::env::var("UQA_HISTORY") {
        if !custom.is_empty() {
            return Some(PathBuf::from(custom));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(
                PathBuf::from(home)
                    .join(".cognica")
                    .join("uqa")
                    .join(HISTORY_FILE),
            );
        }
    }
    None
}

pub(super) fn value_to_display(v: Option<&Value>) -> String {
    match v {
        Some(Value::Null) | None => "NULL".to_string(),
        Some(Value::Void) => String::new(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Int(n)) => n.to_string(),
        Some(Value::Float(f)) => uqa_graph::agtype::format_float_pg(*f),
        Some(Value::Decimal(d)) => d.to_sql_string(),
        Some(Value::Str(s) | Value::FixedChar(s)) => s.clone(),
        // PostgreSQL bytea hex output form.
        Some(Value::Bytes(b)) => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut out = String::new();
            out.push_str("\\x");
            for byte in b {
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
            out
        }
        Some(Value::Temporal(t)) => t.to_sql_string(),
        Some(Value::Json(text) | Value::JsonB(text)) => text.clone(),
        Some(Value::Array(array)) => uqa_sql::expr::array_value_to_string(array),
        Some(Value::List(items)) => pg_array_display(items),
        Some(value @ (Value::Row(_) | Value::Record(_))) => uqa_sql::expr::value_to_string(value),
        // Maps come from JSON/JSONB values: render canonical JSON the
        // way psql prints jsonb, not a Rust-debug-ish map.
        Some(value @ Value::Map(_)) => json_value_display(value),
    }
}

/// `PostgreSQL` array-literal output: `{1,2,3}`, strings quoted when
/// they contain structural characters, `NULL` for nulls, booleans as
/// `t`/`f`, nested arrays recursive.
pub(super) fn pg_array_display(items: &[Value]) -> String {
    fn element(v: &Value) -> String {
        match v {
            Value::Null => "NULL".to_string(),
            Value::Bool(b) => if *b { "t" } else { "f" }.to_string(),
            Value::List(items) => pg_array_display(items),
            Value::Map(_) => json_value_display(v),
            other => {
                let s = value_to_display(Some(other));
                let needs_quotes = s.is_empty()
                    || s.eq_ignore_ascii_case("null")
                    || s.chars()
                        .any(|c| c.is_whitespace() || matches!(c, ',' | '{' | '}' | '"' | '\\'));
                if needs_quotes {
                    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
                } else {
                    s
                }
            }
        }
    }
    let inner: Vec<String> = items.iter().map(element).collect();
    format!("{{{}}}", inner.join(","))
}

/// Canonical JSON rendering for JSON/JSONB values inside result
/// tables: quoted keys, `": "` and `", "` separators, JSON literals
/// for nested nulls - the same shape psql prints for `jsonb`.
pub(super) fn json_value_display(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Void => serde_json::Value::String(String::new()).to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => uqa_graph::agtype::format_float_pg(*f),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) | Value::FixedChar(s) => serde_json::Value::String(s.clone()).to_string(),
        Value::Bytes(_) | Value::Temporal(_) => {
            serde_json::Value::String(value_to_display(Some(v))).to_string()
        }
        Value::Json(text) | Value::JsonB(text) => text.clone(),
        Value::Array(array) => {
            let inner = array
                .elements()
                .iter()
                .map(json_value_display)
                .collect::<Vec<_>>();
            format!("[{}]", inner.join(", "))
        }
        Value::List(items) | Value::Row(items) => {
            let inner: Vec<String> = items.iter().map(json_value_display).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Record(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|(key, value)| {
                    let key = serde_json::Value::String(key.clone()).to_string();
                    format!("{key}: {}", json_value_display(value))
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Value::Map(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, v)| {
                    let key = serde_json::Value::String(k.clone()).to_string();
                    format!("{key}: {}", json_value_display(v))
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        copy_text_cell, print_result_copy_text, print_result_copy_text_with_engine,
        value_to_display,
    };
    use uqa_core::Value;
    use uqa_engine::Engine;
    use uqa_sql::{ColumnType, SQLResult};

    #[test]
    fn float_output_matches_postgresql_special_and_scientific_spelling() {
        assert_eq!(value_to_display(Some(&Value::Float(f64::NAN))), "NaN");
        assert_eq!(
            value_to_display(Some(&Value::Float(f64::INFINITY))),
            "Infinity"
        );
        assert_eq!(
            value_to_display(Some(&Value::Float(f64::NEG_INFINITY))),
            "-Infinity"
        );
        assert_eq!(
            value_to_display(Some(&Value::Float(7.257_415_615_307_999e306))),
            "7.257415615307999e+306"
        );
    }

    #[test]
    fn copy_text_cells_distinguish_null_empty_and_literal_marker() {
        assert_eq!(copy_text_cell(Some(&Value::Null)), "\\N");
        assert_eq!(copy_text_cell(Some(&Value::Str(String::new()))), "");
        assert_eq!(copy_text_cell(Some(&Value::Str("\\N".into()))), "\\\\N");
        assert_eq!(
            copy_text_cell(Some(&Value::Str("a\tb\nc\\d".into()))),
            "a\\tb\\nc\\\\d"
        );
    }

    #[test]
    fn copy_text_uses_postgresql_legacy_vector_output() {
        let result = SQLResult::from_typed_rows_with_positions(
            vec!["proargtypes".into(), "indkey".into()],
            vec![Some(ColumnType::OidVector), Some(ColumnType::Int2Vector)],
            vec![std::collections::BTreeMap::new()],
            Some(vec![vec![
                Value::List(vec![Value::Int(23), Value::Int(25)]),
                Value::List(vec![Value::Int(1), Value::Int(3)]),
            ]]),
        );
        let mut output = Vec::new();
        print_result_copy_text(&result, &mut output);
        assert_eq!(String::from_utf8(output).unwrap(), "23 25\t1 3\n");
    }

    #[test]
    fn copy_text_uses_catalog_aware_regtype_output() {
        let engine = Engine::new();
        let result = engine
            .sql(
                "SELECT 0::regproc, 1598::regproc, 1259::regclass, 11::regnamespace, 23::regtype, ARRAY[0::regtype, 23::regtype, 999999::regtype]",
                &[],
            )
            .unwrap();
        let mut output = Vec::new();
        print_result_copy_text_with_engine(&result, &engine, &mut output);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "-\tpg_catalog.random\tpg_class\tpg_catalog\tinteger\t{-,integer,999999}\n"
        );
    }
}
