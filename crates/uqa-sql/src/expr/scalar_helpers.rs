//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared string, regex, quoting, and point helpers for scalar built-ins.

use super::conversion::to_f64_with_control;
use super::{Result, SQLError, TemporalValue, Value};
use uqa_core::memory::ProductionControl;

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

/// Compile a regex with `PostgreSQL` match-flag behavior.
pub(super) fn compile_pg_regex(
    pattern: &str,
    flags: &str,
    global_allowed: bool,
) -> Result<regex::Regex> {
    #[derive(Clone, Copy)]
    enum Syntax {
        Advanced,
        Basic,
        Quoted,
    }

    let mut case_insensitive = false;
    let mut multi_line = false;
    let mut dot_matches_new_line = true;
    let mut expanded = false;
    let mut syntax = Syntax::Advanced;
    for flag in flags.chars() {
        match flag {
            'g' if global_allowed => {}
            // PostgreSQL 18 clears the composite `REG_ADVANCED` mask after
            // setting `REG_EXTENDED`, which leaves both `b` and `e` using
            // BRE behavior. Match the server's observable behavior exactly.
            'b' | 'e' => syntax = Syntax::Basic,
            'c' => case_insensitive = false,
            'i' => case_insensitive = true,
            'm' | 'n' => {
                multi_line = true;
                dot_matches_new_line = false;
            }
            'p' => {
                multi_line = false;
                dot_matches_new_line = false;
            }
            'q' => syntax = Syntax::Quoted,
            's' => {
                multi_line = false;
                dot_matches_new_line = true;
            }
            't' => expanded = false,
            'w' => {
                multi_line = true;
                dot_matches_new_line = true;
            }
            'x' => expanded = true,
            invalid => {
                return Err(SQLError::Routine {
                    sqlstate: "22023".into(),
                    message: format!("invalid regular expression option: \"{invalid}\""),
                });
            }
        }
    }
    if matches!(syntax, Syntax::Quoted) && (expanded || multi_line || !dot_matches_new_line) {
        return Err(SQLError::Routine {
            sqlstate: "2201B".into(),
            message: "invalid regular expression: invalid argument to regex function".into(),
        });
    }
    let pattern = if expanded {
        expand_postgres_regex(pattern)
    } else {
        pattern.to_string()
    };
    let pattern = match syntax {
        Syntax::Advanced => pattern,
        Syntax::Basic => postgres_basic_regex(&pattern),
        Syntax::Quoted => regex::escape(&pattern),
    };
    let pattern = postgres_character_class_regex(&pattern, !dot_matches_new_line);
    let mut builder = regex::RegexBuilder::new(&pattern);
    builder
        .case_insensitive(case_insensitive)
        .multi_line(multi_line)
        .dot_matches_new_line(dot_matches_new_line);
    builder.build().map_err(|error| SQLError::Routine {
        sqlstate: "2201B".into(),
        message: format!("invalid regular expression: {error}"),
    })
}

fn postgres_character_class_regex(pattern: &str, exclude_newline: bool) -> String {
    let characters = pattern.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(pattern.len());
    let mut position = 0usize;
    let mut in_bracket = false;
    let mut bracket_can_close = false;
    while let Some(&character) = characters.get(position) {
        position += 1;
        if character == '\\' {
            output.push(character);
            if let Some(&escaped) = characters.get(position) {
                position += 1;
                output.push(escaped);
                if in_bracket {
                    bracket_can_close = true;
                }
            }
            continue;
        }
        if !in_bracket {
            output.push(character);
            if character == '[' {
                in_bracket = true;
                bracket_can_close = false;
                if characters.get(position) == Some(&'^') {
                    position += 1;
                    output.push('^');
                    if characters.get(position) == Some(&']') {
                        position += 1;
                        output.push(']');
                        bracket_can_close = true;
                    }
                    if exclude_newline {
                        output.push_str("\\n");
                        if characters.get(position) == Some(&'-') {
                            position += 1;
                            output.push_str("\\-");
                            bracket_can_close = true;
                        }
                    }
                }
            }
            continue;
        }
        if character == '[' && matches!(characters.get(position), Some('.' | ':' | '=')) {
            let delimiter = characters[position];
            output.push(character);
            output.push(delimiter);
            position += 1;
            while let Some(&nested) = characters.get(position) {
                position += 1;
                output.push(nested);
                if nested == delimiter && characters.get(position) == Some(&']') {
                    output.push(']');
                    position += 1;
                    break;
                }
            }
            bracket_can_close = true;
            continue;
        }
        if character == '[' {
            output.push_str("\\[");
            bracket_can_close = true;
            continue;
        }
        output.push(character);
        if character == ']' && bracket_can_close {
            in_bracket = false;
        } else if character != '^' || bracket_can_close {
            bracket_can_close = true;
        }
    }
    output
}

fn expand_postgres_regex(pattern: &str) -> String {
    let mut output = String::with_capacity(pattern.len());
    let mut characters = pattern.chars().peekable();
    let mut in_bracket = false;
    let mut bracket_can_close = false;
    while let Some(character) = characters.next() {
        if character == '\\' {
            output.push(character);
            if let Some(escaped) = characters.next() {
                output.push(escaped);
                if in_bracket {
                    bracket_can_close = true;
                }
            }
            continue;
        }
        if in_bracket {
            if character == '[' {
                if let Some(delimiter @ ('.' | ':' | '=')) = characters.peek().copied() {
                    output.push(character);
                    output.push(delimiter);
                    characters.next();
                    while let Some(nested) = characters.next() {
                        output.push(nested);
                        if nested == delimiter && characters.peek() == Some(&']') {
                            output.push(']');
                            characters.next();
                            break;
                        }
                    }
                    bracket_can_close = true;
                    continue;
                }
            }
            output.push(character);
            if character == ']' && bracket_can_close {
                in_bracket = false;
            } else if character != '^' || bracket_can_close {
                bracket_can_close = true;
            }
            continue;
        }
        match character {
            '[' => {
                in_bracket = true;
                bracket_can_close = false;
                output.push(character);
            }
            '#' => {
                for comment in characters.by_ref() {
                    if comment == '\n' {
                        break;
                    }
                }
            }
            whitespace if postgres_expanded_regex_whitespace(whitespace) => {}
            other => output.push(other),
        }
    }
    output
}

fn postgres_expanded_regex_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{1680}'
            | '\u{2000}'..='\u{2006}'
            | '\u{2008}'..='\u{200A}'
            | '\u{2028}'..='\u{2029}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

fn postgres_basic_regex(pattern: &str) -> String {
    let mut output = String::with_capacity(pattern.len());
    let characters = pattern.chars().collect::<Vec<_>>();
    let mut position = 0usize;
    let mut in_bracket = false;
    let mut bracket_can_close = false;
    let mut at_subexpression_start = true;
    while let Some(&character) = characters.get(position) {
        position += 1;
        if in_bracket {
            if character == '\\' {
                output.push_str(r"\\");
                bracket_can_close = true;
                continue;
            }
            output.push(character);
            if character == ']' && bracket_can_close {
                in_bracket = false;
                at_subexpression_start = false;
            } else if character != '^' || bracket_can_close {
                bracket_can_close = true;
            }
            continue;
        }
        if character == '\\' {
            match characters.get(position).copied() {
                Some('(') => {
                    position += 1;
                    output.push('(');
                    at_subexpression_start = true;
                }
                Some(')') => {
                    position += 1;
                    output.push(')');
                    at_subexpression_start = false;
                }
                Some(bound @ ('{' | '}')) => {
                    position += 1;
                    output.push(bound);
                }
                Some(escaped) if escaped.is_ascii_alphabetic() => {
                    position += 1;
                    output.push(escaped);
                    at_subexpression_start = false;
                }
                Some(escaped) => {
                    position += 1;
                    output.push('\\');
                    output.push(escaped);
                    at_subexpression_start = false;
                }
                None => output.push('\\'),
            }
            continue;
        }
        match character {
            '[' => {
                in_bracket = true;
                bracket_can_close = false;
                output.push(character);
            }
            '^' if at_subexpression_start => output.push(character),
            '^' => {
                output.push_str(r"\^");
                at_subexpression_start = false;
            }
            '$' => {
                let closes_subexpression = matches!(
                    (characters.get(position), characters.get(position + 1)),
                    (Some('\\'), Some(')'))
                );
                if position == characters.len() || closes_subexpression {
                    output.push(character);
                } else {
                    output.push_str(r"\$");
                    at_subexpression_start = false;
                }
            }
            '*' if at_subexpression_start => {
                output.push_str(r"\*");
                at_subexpression_start = false;
            }
            literal @ ('+' | '?' | '(' | ')' | '{' | '}' | '|') => {
                output.push('\\');
                output.push(literal);
                at_subexpression_start = false;
            }
            other => {
                output.push(other);
                at_subexpression_start = false;
            }
        }
    }
    output
}

/// Reserved / type / column-name keywords `PostgreSQL`'s
/// `quote_ident` quotes even when the identifier is otherwise safe.
#[expect(
    clippy::too_many_lines,
    reason = "builtin dispatch preserves arity, NULL, and error precedence"
)]
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
pub(super) fn similar_to_regex(pattern: &str, escape: Option<&str>) -> Result<String> {
    let escape = like_pattern::escape_character(escape)?;
    let mut out = String::with_capacity(pattern.len() + 8);
    out.push_str("^(?:");
    let mut after_escape = false;
    let mut quote_count = 0;
    let mut bracket_depth = 0usize;
    let mut bracket_position = 0usize;
    for character in pattern.chars() {
        if after_escape {
            if character == '"' && bracket_depth == 0 {
                match quote_count {
                    0 => out.push_str("){1,1}?("),
                    1 => out.push_str("){1,1}(?:"),
                    _ => {
                        return Err(SQLError::Routine {
                            sqlstate: "2200C".into(),
                            message: "SQL regular expression may not contain more than two escape-double-quote separators".into(),
                        });
                    }
                }
                quote_count += 1;
            } else {
                push_similar_escaped(&mut out, character);
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
                out.push('\\');
            }
            out.push(character);
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
            '%' => out.push_str(".*"),
            '_' => out.push('.'),
            '[' => {
                bracket_depth = 1;
                bracket_position = 1;
                out.push('[');
            }
            '(' => out.push_str("(?:"),
            '\\' | '.' | '^' | '$' => {
                out.push('\\');
                out.push(character);
            }
            other => out.push(other),
        }
    }
    out.push_str(")$");
    Ok(out)
}

fn push_similar_escaped(output: &mut String, character: char) {
    match character {
        'b' => {
            output.push_str(r"\x08");
            return;
        }
        'B' => {
            output.push_str(r"\\");
            return;
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
        output.push('\\');
    }
    output.push(character);
}

#[cfg(test)]
mod regex_tests;
