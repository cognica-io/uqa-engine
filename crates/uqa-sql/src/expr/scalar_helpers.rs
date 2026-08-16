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

pub(super) fn point_xy(v: &Value) -> Result<(f64, f64)> {
    match v {
        Value::List(items) if items.len() == 2 => Ok((to_f64(&items[0])?, to_f64(&items[1])?)),
        Value::Str(s) | Value::FixedChar(s) => {
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

/// A LIKE/ILIKE pattern compiled once for repeated evaluation.
///
/// ASCII values use a byte matcher without per-row allocation. Unicode keeps
/// SQL's character-oriented `_` semantics and the existing lowercase rules.
pub struct CompiledLikePattern {
    case_insensitive: bool,
    pattern_chars: Vec<char>,
    pattern_ascii: Option<Vec<u8>>,
}

impl CompiledLikePattern {
    #[must_use]
    pub fn new(pattern: &str, case_insensitive: bool) -> Self {
        let normalized = if case_insensitive {
            pattern.to_lowercase()
        } else {
            pattern.to_string()
        };
        let pattern_ascii = normalized
            .is_ascii()
            .then(|| normalized.as_bytes().to_vec());
        let pattern_chars = normalized.chars().collect();
        Self {
            case_insensitive,
            pattern_chars,
            pattern_ascii,
        }
    }

    #[must_use]
    pub fn from_value(pattern: &Value, case_insensitive: bool) -> Self {
        Self::new(&value_to_string(pattern), case_insensitive)
    }

    #[must_use]
    pub fn is_match(&self, haystack: &str) -> bool {
        if self.case_insensitive {
            let normalized = haystack.to_lowercase();
            if let Some(pattern) = self
                .pattern_ascii
                .as_deref()
                .filter(|_| normalized.is_ascii())
            {
                return wildcard_match(normalized.as_bytes(), pattern, b'%', b'_');
            }
            let haystack = normalized.chars().collect::<Vec<_>>();
            return wildcard_match(&haystack, &self.pattern_chars, '%', '_');
        }
        if let Some(pattern) = self
            .pattern_ascii
            .as_deref()
            .filter(|_| haystack.is_ascii())
        {
            return wildcard_match(haystack.as_bytes(), pattern, b'%', b'_');
        }
        let haystack = haystack.chars().collect::<Vec<_>>();
        wildcard_match(&haystack, &self.pattern_chars, '%', '_')
    }

    #[must_use]
    pub fn matches_value(&self, haystack: &Value) -> bool {
        match haystack {
            Value::Str(text) => self.is_match(text),
            Value::FixedChar(text) => self.is_match(text.trim_end_matches(' ')),
            Value::Null => self.is_match(""),
            other => self.is_match(&value_to_string(other)),
        }
    }
}

fn wildcard_match<T: Copy + Eq>(
    haystack: &[T],
    pattern: &[T],
    wildcard_many: T,
    wildcard_one: T,
) -> bool {
    let mut haystack_index = 0;
    let mut pattern_index = 0;
    let mut star: Option<(usize, usize)> = None;
    while haystack_index < haystack.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == wildcard_one
                || pattern[pattern_index] == haystack[haystack_index])
        {
            haystack_index += 1;
            pattern_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == wildcard_many {
            star = Some((pattern_index, haystack_index));
            pattern_index += 1;
        } else if let Some((star_pattern, star_haystack)) = star {
            pattern_index = star_pattern + 1;
            haystack_index = star_haystack + 1;
            star = Some((star_pattern, star_haystack + 1));
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == wildcard_many {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

pub(super) fn like_match(haystack: &str, pattern: &str, case_insensitive: bool) -> bool {
    CompiledLikePattern::new(pattern, case_insensitive).is_match(haystack)
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
pub fn quote_ident(ident: &str) -> String {
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

#[cfg(test)]
mod regex_tests {
    use super::compile_pg_regex;

    #[test]
    fn postgres_regex_flags_control_expansion_quoting_and_newlines() {
        assert!(compile_pg_regex("a b", "x", false).unwrap().is_match("ab"));
        assert!(!compile_pg_regex("a b", "x", false).unwrap().is_match("a b"));
        assert!(compile_pg_regex("a.b", "q", false).unwrap().is_match("a.b"));
        assert!(compile_pg_regex("a.b", "", false).unwrap().is_match("a\nb"));
        assert!(!compile_pg_regex("a.b", "n", false)
            .unwrap()
            .is_match("a\nb"));
        assert!(!compile_pg_regex("[^a]", "n", false).unwrap().is_match("\n"));
        assert!(compile_pg_regex("[^]a]", "n", false).unwrap().is_match("b"));
        assert!(!compile_pg_regex("[^]a]", "n", false).unwrap().is_match("]"));
        assert!(!compile_pg_regex("[^]a]", "n", false)
            .unwrap()
            .is_match("\n"));
        assert!(!compile_pg_regex(r"[^\n]", "en", false)
            .unwrap()
            .is_match("\n"));
        for flags in ["m", "n", "p"] {
            assert!(compile_pg_regex("[^-a]", flags, false)
                .unwrap()
                .is_match("1"));
            assert!(!compile_pg_regex("[^-a]", flags, false)
                .unwrap()
                .is_match("\n"));
        }
        for flags in ["", "n"] {
            assert!(compile_pg_regex("[[]", flags, false).unwrap().is_match("["));
            assert!(compile_pg_regex("[^[]", flags, false)
                .unwrap()
                .is_match("1"));
            assert!(!compile_pg_regex("[^[]", flags, false)
                .unwrap()
                .is_match("["));
        }
        assert!(compile_pg_regex("[^a]", "s", false).unwrap().is_match("\n"));
        assert!(compile_pg_regex("[ ]", "x", false).unwrap().is_match(" "));
        assert!(compile_pg_regex("[[:digit:] ]", "x", false)
            .unwrap()
            .is_match(" "));
        assert!(compile_pg_regex("[[:digit:]#]", "x", false)
            .unwrap()
            .is_match("#"));
        assert!(compile_pg_regex("a # ignored\n b", "x", false)
            .unwrap()
            .is_match("ab"));
        for literal in ['\u{0085}', '\u{00A0}', '\u{2007}', '\u{202F}'] {
            let pattern = format!("a{literal}b");
            assert!(!compile_pg_regex(&pattern, "x", false)
                .unwrap()
                .is_match("ab"));
            assert!(compile_pg_regex(&pattern, "x", false)
                .unwrap()
                .is_match(&pattern));
        }
        assert!(compile_pg_regex("a\u{2003}b", "x", false)
            .unwrap()
            .is_match("ab"));
        for syntax in ["b", "e"] {
            assert!(compile_pg_regex("a+", syntax, false)
                .unwrap()
                .is_match("a+"));
            assert!(!compile_pg_regex("a+", syntax, false)
                .unwrap()
                .is_match("aa"));
        }
        assert!(compile_pg_regex(r"a\{1,\}", "b", false)
            .unwrap()
            .is_match("aa"));
        assert!(compile_pg_regex(r"\d", "b", false).unwrap().is_match("d"));
        assert!(!compile_pg_regex(r"\d", "b", false).unwrap().is_match("1"));
        for syntax in ["b", "e"] {
            assert!(compile_pg_regex("a^b", syntax, false)
                .unwrap()
                .is_match("a^b"));
            assert!(compile_pg_regex("a$b", syntax, false)
                .unwrap()
                .is_match("a$b"));
            assert!(compile_pg_regex("^ab$", syntax, false)
                .unwrap()
                .is_match("ab"));
        }
    }

    #[test]
    fn postgres_regex_rejects_invalid_options_with_pg_sqlstate() {
        let error = compile_pg_regex("a", "z", false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22023"));
        let error = compile_pg_regex("a", "g", false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22023"));
        for flags in ["qn", "qp", "qw", "qx"] {
            let error = compile_pg_regex("a", flags, false).unwrap_err();
            assert_eq!(error.sqlstate(), Some("2201B"));
        }
        assert!(compile_pg_regex("a", "qns", false).unwrap().is_match("a"));
    }
}
