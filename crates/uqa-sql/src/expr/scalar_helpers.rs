//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared string, regex, quoting, and point helpers for scalar built-ins.

use super::{to_f64, value_to_string, Result, SQLError, TemporalValue, Value};

// --------------------------------------------------------------------
// JSON helpers
// --------------------------------------------------------------------

pub(super) fn typeof_value(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Int(_) => "integer".into(),
        Value::Float(_) => "double precision".into(),
        Value::Decimal(_) => "numeric".into(),
        Value::Str(_) => "text".into(),
        Value::Bytes(_) => "bytea".into(),
        Value::Temporal(value) => match value {
            TemporalValue::Date { .. } => "date".into(),
            TemporalValue::Time { .. } => "time without time zone".into(),
            TemporalValue::TimeTz { .. } => "time with time zone".into(),
            TemporalValue::Timestamp { .. } => "timestamp without time zone".into(),
            TemporalValue::TimestampTz { .. } => "timestamp with time zone".into(),
            TemporalValue::Interval { .. } => "interval".into(),
        },
        Value::List(_) => "array".into(),
        Value::Map(_) => "jsonb".into(),
    }
}

pub(super) fn point_xy(v: &Value) -> Result<(f64, f64)> {
    match v {
        Value::List(items) if items.len() == 2 => Ok((to_f64(&items[0])?, to_f64(&items[1])?)),
        Value::Str(s) => {
            let cleaned = s.trim_matches(|c: char| c == '(' || c == ')' || c == '[' || c == ']');
            let parts: Vec<&str> = cleaned.split(',').map(str::trim).collect();
            if parts.len() != 2 {
                return Err(SQLError::TypeMismatch(format!("point: cannot parse {s:?}")));
            }
            let x: f64 = parts[0]
                .parse()
                .map_err(|e| SQLError::TypeMismatch(format!("point.x: {e}")))?;
            let y: f64 = parts[1]
                .parse()
                .map_err(|e| SQLError::TypeMismatch(format!("point.y: {e}")))?;
            Ok((x, y))
        }
        other => Err(SQLError::TypeMismatch(format!(
            "point: not coercible {other:?}"
        ))),
    }
}

pub(super) fn like_match(haystack: &str, pattern: &str, case_insensitive: bool) -> bool {
    let h: Vec<char> = if case_insensitive {
        haystack.to_lowercase().chars().collect()
    } else {
        haystack.chars().collect()
    };
    let p: Vec<char> = if case_insensitive {
        pattern.to_lowercase().chars().collect()
    } else {
        pattern.chars().collect()
    };
    fn rec(h: &[char], p: &[char]) -> bool {
        let mut hi = 0;
        let mut pi = 0;
        let mut star: Option<(usize, usize)> = None;
        while hi < h.len() {
            if pi < p.len() && (p[pi] == '_' || p[pi] == h[hi]) {
                hi += 1;
                pi += 1;
            } else if pi < p.len() && p[pi] == '%' {
                star = Some((pi, hi));
                pi += 1;
            } else if let Some((spi, shi)) = star {
                pi = spi + 1;
                hi = shi + 1;
                star = Some((spi, shi + 1));
            } else {
                return false;
            }
        }
        while pi < p.len() && p[pi] == '%' {
            pi += 1;
        }
        pi == p.len()
    }
    rec(&h, &p)
}

/// `trim` / `ltrim` / `rtrim` / `btrim` with the optional
/// character-SET second argument (defaults to whitespace).
pub(super) fn trim_chars(args: &[Value], start: bool, end: bool) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(SQLError::TypeMismatch("trim takes 1-2 args".into()));
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return Ok(Value::Null);
    }
    let s = value_to_string(&args[0]);
    let out = match args.get(1) {
        None => match (start, end) {
            (true, true) => s.trim(),
            (true, false) => s.trim_start(),
            (false, true) => s.trim_end(),
            (false, false) => s.as_str(),
        }
        .to_string(),
        Some(set) => {
            let set: Vec<char> = value_to_string(set).chars().collect();
            let matches_set = |c: char| set.contains(&c);
            let mut out = s.as_str();
            if start {
                out = out.trim_start_matches(matches_set);
            }
            if end {
                out = out.trim_end_matches(matches_set);
            }
            out.to_string()
        }
    };
    Ok(Value::Str(out))
}

/// Compile a POSIX-ish regex with `PostgreSQL` match flags (`i`, `n`).
pub(super) fn compile_pg_regex(pattern: &str, flags: &str) -> Result<regex::Regex> {
    let mut prefix = String::new();
    if flags.contains('i') {
        prefix.push_str("(?i)");
    }
    if flags.contains('n') || flags.contains('m') {
        prefix.push_str("(?m)");
    }
    if flags.contains('s') {
        prefix.push_str("(?s)");
    }
    regex::Regex::new(&format!("{prefix}{pattern}"))
        .map_err(|e| SQLError::TypeMismatch(format!("regex: {e}")))
}

/// Reserved / type / column-name keywords `PostgreSQL`'s
/// `quote_ident` quotes even when the identifier is otherwise safe.
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

/// `quote_ident`: double-quote unless the identifier is a safe
/// lower-case name that is not a keyword.
pub(super) fn quote_ident(ident: &str) -> String {
    let safe = !ident.is_empty()
        && ident.chars().enumerate().all(|(i, c)| {
            c.is_ascii_lowercase() || c == '_' || (i > 0 && (c.is_ascii_digit() || c == '$'))
        });
    if safe && !is_quoted_keyword(ident) {
        return ident.to_string();
    }
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// `quote_literal`: single-quote with doubled quotes; backslashes
/// switch to the `E'...'` form with doubled backslashes.
pub(super) fn quote_literal(text: &str) -> String {
    let escaped = text.replace('\'', "''");
    if escaped.contains('\\') {
        format!("E'{}'", escaped.replace('\\', "\\\\"))
    } else {
        format!("'{escaped}'")
    }
}

/// Translate a SQL `SIMILAR TO` pattern into an anchored regex:
/// `%` -> `.*`, `_` -> `.`, regex metacharacters that SQL regexes
/// treat literally get escaped, and `(|)*+?{}[]` pass through.
pub(super) fn similar_to_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    out.push_str("^(?:");
    let mut chars = pattern.chars().peekable();
    let mut in_brackets = false;
    while let Some(c) = chars.next() {
        if in_brackets {
            out.push(c);
            if c == ']' {
                in_brackets = false;
            }
            continue;
        }
        match c {
            '%' => out.push_str(".*"),
            '_' => out.push('.'),
            '[' => {
                in_brackets = true;
                out.push('[');
            }
            '\\' => {
                // Default SIMILAR TO escape: the next character is
                // literal.
                if let Some(next) = chars.next() {
                    for e in regex::escape(&next.to_string()).chars() {
                        out.push(e);
                    }
                }
            }
            '.' | '^' | '$' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out.push_str(")$");
    out
}
