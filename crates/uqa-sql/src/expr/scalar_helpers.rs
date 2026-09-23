//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared string, regex, quoting, and point helpers for scalar built-ins.

use super::conversion::to_f64_with_control;
use super::{Result, SQLError, TemporalValue, Value};
use uqa_core::memory::{Produced, ProductionControl, ProductionString};

mod quoting;
pub use quoting::quote_ident;
pub(super) use quoting::{quote_ident_with_control, quote_literal_with_control};

// --------------------------------------------------------------------
// JSON helpers
// --------------------------------------------------------------------

pub(super) fn typeof_value(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Void => "void".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Int(_) => "integer".into(),
        Value::Float(_) => "double precision".into(),
        Value::Decimal(_) => "numeric".into(),
        Value::Str(_) => "text".into(),
        Value::FixedChar(_) => "character".into(),
        Value::Bytes(_) => "bytea".into(),
        Value::Temporal(value) => match value {
            TemporalValue::Date { .. } => "date".into(),
            TemporalValue::Time { .. } => "time without time zone".into(),
            TemporalValue::TimeTz { .. } => "time with time zone".into(),
            TemporalValue::Timestamp { .. } => "timestamp without time zone".into(),
            TemporalValue::TimestampTz { .. } => "timestamp with time zone".into(),
            TemporalValue::Interval { .. } => "interval".into(),
        },
        Value::Json(_) => "json".into(),
        Value::JsonB(_) => "jsonb".into(),
        Value::Array(_) => "array".into(),
        Value::List(_) => "array".into(),
        Value::Row(_) | Value::Record(_) => "record".into(),
        Value::Map(_) => "jsonb".into(),
    }
}

pub(super) fn point_xy(v: &Value, control: &ProductionControl<'_>) -> Result<(f64, f64)> {
    control.check()?;
    match v {
        Value::List(items) if items.len() == 2 => Ok((
            to_f64_with_control(&items[0], control)?,
            to_f64_with_control(&items[1], control)?,
        )),
        Value::Str(s) | Value::FixedChar(s) => {
            let cleaned = s.trim_matches(|c: char| c == '(' || c == ')' || c == '[' || c == ']');
            let mut parts = cleaned.split(',').map(str::trim);
            let (Some(x), Some(y), None) = (parts.next(), parts.next(), parts.next()) else {
                return Err(SQLError::TypeMismatch(format!("point: cannot parse {s:?}")));
            };
            control.check()?;
            let x: f64 = x
                .parse()
                .map_err(|e| SQLError::TypeMismatch(format!("point.x: {e}")))?;
            control.check()?;
            let y: f64 = y
                .parse()
                .map_err(|e| SQLError::TypeMismatch(format!("point.y: {e}")))?;
            control.check()?;
            Ok((x, y))
        }
        other => Err(SQLError::TypeMismatch(format!(
            "point: not coercible {other:?}"
        ))),
    }
}

pub(super) mod casing;
mod like_pattern;
pub use like_pattern::CompiledLikePattern;

#[cfg(test)]
fn compile_pg_regex(
    pattern: &str,
    flags: &str,
    global_allowed: bool,
) -> Result<super::regex::LocalRegex> {
    super::regex::compile(
        pattern,
        flags,
        global_allowed,
        &ProductionControl::uncontrolled(),
    )
}

pub(super) fn is_quoted_keyword(word: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "all",
        "analyse",
        "analyze",
        "and",
        "any",
        "array",
        "as",
        "asc",
        "asymmetric",
        "authorization",
        "between",
        "bigint",
        "binary",
        "bit",
        "boolean",
        "both",
        "case",
        "cast",
        "char",
        "character",
        "check",
        "coalesce",
        "collate",
        "collation",
        "column",
        "concurrently",
        "constraint",
        "create",
        "cross",
        "current_catalog",
        "current_date",
        "current_role",
        "current_schema",
        "current_time",
        "current_timestamp",
        "current_user",
        "dec",
        "decimal",
        "default",
        "deferrable",
        "desc",
        "distinct",
        "do",
        "else",
        "end",
        "except",
        "exists",
        "extract",
        "false",
        "fetch",
        "float",
        "for",
        "foreign",
        "freeze",
        "from",
        "full",
        "grant",
        "greatest",
        "group",
        "grouping",
        "having",
        "ilike",
        "in",
        "initially",
        "inner",
        "inout",
        "int",
        "integer",
        "intersect",
        "interval",
        "into",
        "is",
        "isnull",
        "join",
        "json",
        "json_array",
        "json_arrayagg",
        "json_exists",
        "json_object",
        "json_objectagg",
        "json_query",
        "json_scalar",
        "json_serialize",
        "json_table",
        "json_value",
        "lateral",
        "leading",
        "least",
        "left",
        "like",
        "limit",
        "localtime",
        "localtimestamp",
        "merge_action",
        "national",
        "natural",
        "nchar",
        "none",
        "normalize",
        "not",
        "notnull",
        "null",
        "nullif",
        "numeric",
        "offset",
        "on",
        "only",
        "or",
        "order",
        "out",
        "outer",
        "overlaps",
        "overlay",
        "placing",
        "position",
        "precision",
        "primary",
        "real",
        "references",
        "returning",
        "right",
        "row",
        "select",
        "session_user",
        "setof",
        "similar",
        "smallint",
        "some",
        "substring",
        "symmetric",
        "system_user",
        "table",
        "tablesample",
        "then",
        "time",
        "timestamp",
        "to",
        "trailing",
        "treat",
        "trim",
        "true",
        "union",
        "unique",
        "user",
        "using",
        "values",
        "varchar",
        "variadic",
        "verbose",
        "when",
        "where",
        "window",
        "with",
        "xmlattributes",
        "xmlconcat",
        "xmlelement",
        "xmlexists",
        "xmlforest",
        "xmlnamespaces",
        "xmlparse",
        "xmlpi",
        "xmlroot",
        "xmlserialize",
        "xmltable",
    ];
    KEYWORDS.binary_search(&word).is_ok()
}

/// Translate a SQL `SIMILAR TO` pattern into an anchored PostgreSQL-style regex.
/// `None` selects the default backslash escape and `Some("")` disables escaping.
pub(super) fn similar_to_regex_with_control(
    pattern: &str,
    escape: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    control.check()?;
    let escape = like_pattern::escape_character(escape)?;
    let mut out = ProductionString::new(*control);
    out.push_str("^(?:")?;
    let mut after_escape = false;
    let mut quote_count = 0;
    let mut bracket_depth = 0usize;
    let mut bracket_position = 0usize;
    for character in pattern.chars() {
        if after_escape {
            if character == '"' && bracket_depth == 0 {
                match quote_count {
                    0 => out.push_str("){1,1}?(")?,
                    1 => out.push_str("){1,1}(?:")?,
                    _ => {
                        return Err(SQLError::Routine {
                            sqlstate: "2200C".into(),
                            message: "SQL regular expression may not contain more than two escape-double-quote separators".into(),
                        });
                    }
                }
                quote_count += 1;
            } else {
                push_similar_escaped(&mut out, character)?;
                bracket_position = 3;
            }
            after_escape = false;
            continue;
        }
        if escape == Some(character) {
            after_escape = true;
            continue;
        }
        if bracket_depth > 0 {
            if character == '\\' && escape != Some('\\') {
                out.push('\\')?;
            }
            out.push(character)?;
            if character == ']' && bracket_position > 2 {
                bracket_depth -= 1;
            } else if character == '[' {
                bracket_depth += 1;
                bracket_position = 3;
            } else if character == '^' {
                bracket_position += 1;
            } else {
                bracket_position = 3;
            }
            continue;
        }
        match character {
            '%' => out.push_str(".*")?,
            '_' => out.push('.')?,
            '[' => {
                bracket_depth = 1;
                bracket_position = 1;
                out.push('[')?;
            }
            '(' => out.push_str("(?:")?,
            '\\' | '.' | '^' | '$' => {
                out.push('\\')?;
                out.push(character)?;
            }
            other => out.push(other)?,
        }
    }
    out.push_str(")$")?;
    Ok(out.finish()?)
}

fn push_similar_escaped(output: &mut ProductionString<'_>, character: char) -> Result<()> {
    match character {
        'b' => {
            output.push_str(r"\x08")?;
            return Ok(());
        }
        'B' => {
            output.push_str(r"\\")?;
            return Ok(());
        }
        _ => {}
    }
    if character.is_ascii_alphanumeric()
        || matches!(
            character,
            '\\' | '.'
                | '^'
                | '$'
                | '|'
                | '?'
                | '*'
                | '+'
                | '('
                | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | '-'
        )
    {
        output.push('\\')?;
    }
    output.push(character)?;
    Ok(())
}

#[cfg(test)]
mod regex_tests;
