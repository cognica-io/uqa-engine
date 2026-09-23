//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing regular-expression semantics and native compilation owner.

use crate::{
    error::{Result, SQLError},
    expr::{compile_pg_regex, to_i64, value_to_string},
};
use uqa_core::{ArrayValue, Value};

pub(super) fn evaluate(name: &str, args: &[Value]) -> Result<Value> {
    match name {
        "regexp_match" | "regexp_matches" => captures(name, args),
        "regexp_replace" => replace(args),
        _ => unreachable!("regular-expression family membership was checked"),
    }
}

fn captures(name: &str, args: &[Value]) -> Result<Value> {
    if args.len() < 2 || args.len() > 3 {
        return Err(SQLError::TypeMismatch(
            "regexp_match takes 2 or 3 args".into(),
        ));
    }
    let s = value_to_string(&args[0]);
    let pat = value_to_string(&args[1]);
    let flags = args.get(2).map(value_to_string).unwrap_or_default();
    let re = compile_pg_regex(&pat, &flags, name == "regexp_matches")?;
    match re.captures(&s) {
        None => Ok(Value::Null),
        Some(caps) => {
            // regexp_match returns text[]: the capture groups,
            // or the whole match as a one-element array when
            // the pattern has no groups (PostgreSQL).
            let groups: Vec<Value> = caps
                .iter()
                .skip(1)
                .map(|m| {
                    m.map(|x| Value::Str(x.as_str().into()))
                        .unwrap_or(Value::Null)
                })
                .collect();
            if groups.is_empty() {
                let full_match = caps.get(0).ok_or_else(|| {
                    SQLError::Internal("regex capture set omitted its mandatory full match".into())
                })?;
                ArrayValue::try_new(vec![Value::Str(full_match.as_str().into())])
                    .map(Value::Array)
                    .ok_or_else(|| SQLError::TypeMismatch("invalid regexp_match result".into()))
            } else {
                ArrayValue::try_new(groups)
                    .map(Value::Array)
                    .ok_or_else(|| SQLError::TypeMismatch("invalid regexp_match result".into()))
            }
        }
    }
}

fn replace(args: &[Value]) -> Result<Value> {
    if !(3..=6).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "regexp_replace takes 3 to 6 args".into(),
        ));
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return Ok(Value::Null);
    }
    let s = value_to_string(&args[0]);
    let pat = value_to_string(&args[1]);
    let repl = value_to_string(&args[2]);
    let (start, occurrence, flags) = match args {
        [_, _, _] => (1, 1, String::new()),
        [_, _, _, Value::Str(flags) | Value::FixedChar(flags)] => {
            (1, usize::from(!flags.contains('g')), flags.clone())
        }
        [_, _, _, start] => (
            positive_regexp_replace_parameter(start, "start")?,
            1,
            String::new(),
        ),
        [_, _, _, start, occurrence] => (
            positive_regexp_replace_parameter(start, "start")?,
            nonnegative_regexp_replace_parameter(occurrence, "N")?,
            String::new(),
        ),
        [_, _, _, start, occurrence, flags] => (
            positive_regexp_replace_parameter(start, "start")?,
            nonnegative_regexp_replace_parameter(occurrence, "N")?,
            value_to_string(flags),
        ),
        _ => unreachable!("regexp_replace arity was checked"),
    };
    regexp_replace(&s, &pat, &repl, start, occurrence, &flags).map(Value::Str)
}

fn invalid_regexp_replace_parameter(name: &str, value: i64) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("invalid value for parameter \"{name}\": {value}"),
    }
}

fn positive_regexp_replace_parameter(value: &Value, name: &str) -> Result<usize> {
    let value = to_i64(value)?;
    if value <= 0 {
        return Err(invalid_regexp_replace_parameter(name, value));
    }
    Ok(usize::try_from(value).unwrap_or(usize::MAX))
}

fn nonnegative_regexp_replace_parameter(value: &Value, name: &str) -> Result<usize> {
    let value = to_i64(value)?;
    if value < 0 {
        return Err(invalid_regexp_replace_parameter(name, value));
    }
    Ok(usize::try_from(value).unwrap_or(usize::MAX))
}

fn regexp_replace(
    string: &str,
    pattern: &str,
    replacement: &str,
    start: usize,
    occurrence: usize,
    flags: &str,
) -> Result<String> {
    let base_chars = start - 1;
    let char_count = string.chars().count();
    if base_chars > char_count {
        return Ok(string.to_string());
    }
    let byte_start = if base_chars == char_count {
        string.len()
    } else {
        string
            .char_indices()
            .nth(base_chars)
            .map(|(index, _)| index)
            .unwrap_or(string.len())
    };
    let (prefix, tail) = string.split_at(byte_start);
    let regex = compile_pg_regex(pattern, flags, true)?;
    let replacement = postgres_regex_replacement(replacement);
    let replaced = if occurrence == 0 {
        regex.replace_all(tail, replacement.as_str()).into_owned()
    } else if let Some(captures) = regex.captures_iter(tail).nth(occurrence - 1) {
        let matched = captures.get(0).ok_or_else(|| {
            SQLError::Internal("regex capture set omitted its mandatory full match".into())
        })?;
        let mut expanded = String::new();
        captures.expand(&replacement, &mut expanded);
        let mut output = String::with_capacity(tail.len() + expanded.len());
        output.push_str(&tail[..matched.start()]);
        output.push_str(&expanded);
        output.push_str(&tail[matched.end()..]);
        output
    } else {
        tail.to_string()
    };
    Ok(format!("{prefix}{replaced}"))
}

fn postgres_regex_replacement(replacement: &str) -> String {
    let mut output = String::with_capacity(replacement.len());
    let mut characters = replacement.chars();
    while let Some(character) = characters.next() {
        match character {
            '$' => output.push_str("$$"),
            '\\' => match characters.next() {
                Some(digit @ '1'..='9') => {
                    output.push('$');
                    output.push(digit);
                }
                Some('&') => output.push_str("$0"),
                Some('\\') => output.push('\\'),
                Some(other) => {
                    output.push('\\');
                    output.push(other);
                }
                None => output.push('\\'),
            },
            other => output.push(other),
        }
    }
    output
}
