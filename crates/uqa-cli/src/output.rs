//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tabular and expanded query-result rendering.

use super::{PathBuf, SQLResult, Value, Write, HISTORY_FILE};

/// Render the result one column per line per row -- mirrors
/// `PostgreSQL` `psql`'s `\x` expanded display mode.
pub(super) fn print_result_expanded(result: &SQLResult, out: &mut (impl Write + ?Sized)) {
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
    for (idx, row) in result.rows.iter().enumerate() {
        let _ = writeln!(out, "-[ RECORD {} ]-", idx + 1);
        for col in &columns {
            let value = value_to_display(row.get(col));
            let _ = writeln!(out, "{col:<label_width$} | {value}");
        }
    }
    let _ = writeln!(out, "({} row(s))", result.rows.len());
}

pub(super) fn print_result(result: &SQLResult, out: &mut (impl Write + ?Sized)) {
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
        .map(|row| {
            columns
                .iter()
                .map(|c| value_to_display(row.get(c)))
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
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Int(n)) => n.to_string(),
        Some(Value::Float(f)) => format!("{f}"),
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
        Some(Value::List(items)) => pg_array_display(items),
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
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) | Value::FixedChar(s) => serde_json::Value::String(s.clone()).to_string(),
        Value::Bytes(_) | Value::Temporal(_) => {
            serde_json::Value::String(value_to_display(Some(v))).to_string()
        }
        Value::Json(text) | Value::JsonB(text) => text.clone(),
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(json_value_display).collect();
            format!("[{}]", inner.join(", "))
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
