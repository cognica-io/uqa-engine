//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core control, string, regex, and basic numeric built-ins.

use super::{
    compare, division_by_zero, expect_str, float1, gcd_i64, initcap_str, json_concat, like_match,
    out_of_range, string1, to_decimal, to_f64, to_i64, trim_chars, value_to_string, values_equal,
    Result, SQLError, Value,
};

pub(super) fn eval_core_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    const NAMES: &[&str] = &[
        "coalesce",
        "nullif",
        "greatest",
        "least",
        "upper",
        "lower",
        "length",
        "char_length",
        "character_length",
        "octet_length",
        "trim",
        "btrim",
        "ltrim",
        "rtrim",
        "initcap",
        "reverse",
        "concat",
        "concat_op",
        "concat_ws",
        "replace",
        "substring",
        "substr",
        "left",
        "right",
        "abs",
        "round",
        "ceil",
        "ceiling",
        "floor",
        "power",
        "pow",
        "sqrt",
        "mod",
        "div",
        "gcd",
        "lcm",
        "starts_with",
        "position",
        "strpos",
        "ascii",
        "like",
        "ilike",
        "chr",
        "regexp_match",
        "regexp_matches",
        "regexp_replace",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Value> {
        match name {
            "coalesce" => {
                for a in args {
                    if !matches!(a, Value::Null) {
                        return Ok(a.clone());
                    }
                }
                Ok(Value::Null)
            }
            "nullif" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("nullif takes 2 args".into()));
                }
                if values_equal(&args[0], &args[1]) {
                    Ok(Value::Null)
                } else {
                    Ok(args[0].clone())
                }
            }
            "greatest" => {
                let mut best: Option<&Value> = None;
                for a in args {
                    if matches!(a, Value::Null) {
                        continue;
                    }
                    best = Some(match best {
                        None => a,
                        Some(prev) => {
                            if compare(a, prev)?.is_gt() {
                                a
                            } else {
                                prev
                            }
                        }
                    });
                }
                Ok(best.cloned().unwrap_or(Value::Null))
            }
            "least" => {
                let mut best: Option<&Value> = None;
                for a in args {
                    if matches!(a, Value::Null) {
                        continue;
                    }
                    best = Some(match best {
                        None => a,
                        Some(prev) => {
                            if compare(a, prev)?.is_lt() {
                                a
                            } else {
                                prev
                            }
                        }
                    });
                }
                Ok(best.cloned().unwrap_or(Value::Null))
            }
            "upper" => string1(args, |s| s.to_uppercase()),
            "lower" => string1(args, |s| s.to_lowercase()),
            "length" | "char_length" | "character_length" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = expect_str(args, 0)?;
                Ok(Value::Int(s.chars().count() as i64))
            }
            "octet_length" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = expect_str(args, 0)?;
                Ok(Value::Int(s.len() as i64))
            }
            // trim family: the optional second argument is a SET of
            // characters to strip (PostgreSQL semantics), not a substring.
            "trim" | "btrim" => trim_chars(args, true, true),
            "ltrim" => trim_chars(args, true, false),
            "rtrim" => trim_chars(args, false, true),
            "initcap" => string1(args, initcap_str),
            "reverse" => string1(args, |s| s.chars().rev().collect()),
            "concat" => {
                // PostgreSQL `CONCAT()` skips NULLs.
                let mut buf = String::new();
                for a in args {
                    if matches!(a, Value::Null) {
                        continue;
                    }
                    buf.push_str(&value_to_string(a));
                }
                Ok(Value::Str(buf))
            }
            "concat_op" => {
                // SQL `||` operator: NULL propagates. Argument count is
                // always two because the parser only emits this when
                // rewriting a binary expression.
                for a in args {
                    if matches!(a, Value::Null) {
                        return Ok(Value::Null);
                    }
                }
                if let Some(value) = json_concat(args)? {
                    return Ok(value);
                }
                let mut buf = String::new();
                for a in args {
                    buf.push_str(&value_to_string(a));
                }
                Ok(Value::Str(buf))
            }
            "concat_ws" => {
                if args.is_empty() {
                    return Err(SQLError::TypeMismatch("concat_ws needs separator".into()));
                }
                let sep = match &args[0] {
                    Value::Null => return Ok(Value::Null),
                    other => value_to_string(other),
                };
                let mut parts: Vec<String> = Vec::new();
                for a in &args[1..] {
                    if matches!(a, Value::Null) {
                        continue;
                    }
                    parts.push(value_to_string(a));
                }
                Ok(Value::Str(parts.join(&sep)))
            }
            "replace" => {
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("replace takes 3 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let from = value_to_string(&args[1]);
                let to = value_to_string(&args[2]);
                Ok(Value::Str(s.replace(&from, &to)))
            }
            "substring" | "substr" => {
                // SUBSTRING(string, start [, length]). 1-indexed; a start
                // before 1 clips the window against the string
                // (`substring('hello', -1, 3)` = 'h') and a negative
                // length errors, per PostgreSQL.
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch("substring takes 2-3 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let start = to_i64(&args[1])?;
                let chars: Vec<char> = s.chars().collect();
                let n = chars.len() as i64;
                let end_exclusive = if args.len() == 3 {
                    let len = to_i64(&args[2])?;
                    if len < 0 {
                        return Err(SQLError::Routine {
                            sqlstate: "22011".into(),
                            message: "negative substring length not allowed".into(),
                        });
                    }
                    start
                        .checked_add(len)
                        .ok_or_else(|| out_of_range("bigint"))?
                } else {
                    i64::MAX
                };
                let begin = start.max(1).min(n + 1);
                let end = end_exclusive.clamp(1, n + 1);
                if end <= begin {
                    return Ok(Value::Str(String::new()));
                }
                let slice: String = chars[(begin - 1) as usize..(end - 1) as usize]
                    .iter()
                    .collect();
                Ok(Value::Str(slice))
            }
            "left" => {
                // left(s, -n) drops the last n characters (PostgreSQL).
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("left takes 2 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let n = to_i64(&args[1])?;
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len() as i64;
                let take = if n >= 0 { n.min(len) } else { (len + n).max(0) } as usize;
                Ok(Value::Str(chars[..take].iter().collect()))
            }
            "right" => {
                // right(s, -n) drops the first n characters (PostgreSQL).
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("right takes 2 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let n = to_i64(&args[1])?;
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len() as i64;
                let take = if n >= 0 { n.min(len) } else { (len + n).max(0) } as usize;
                let start = chars.len() - take;
                Ok(Value::Str(chars[start..].iter().collect()))
            }
            "abs" => match &args[0] {
                Value::Int(i) => i
                    .checked_abs()
                    .map(Value::Int)
                    .ok_or_else(|| out_of_range("bigint")),
                Value::Float(f) => Ok(Value::Float(f.abs())),
                Value::Decimal(d) => Ok(Value::Decimal(d.abs())),
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!(
                    "abs() expected number, got {other:?}"
                ))),
            },
            "round" => match args.len() {
                1 => match &args[0] {
                    Value::Int(i) => Ok(Value::Int(*i)),
                    // float8 rounding is round-half-to-even (rint);
                    // numeric rounding is half-away-from-zero.
                    Value::Float(f) => Ok(Value::Float(f.round_ties_even())),
                    Value::Decimal(d) => Ok(Value::Decimal(d.round_dp(0))),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!("round({other:?})"))),
                },
                2 => {
                    if args.iter().any(|arg| matches!(arg, Value::Null)) {
                        return Ok(Value::Null);
                    }
                    if matches!(args[0], Value::Decimal(_)) {
                        let places = to_i64(&args[1])?;
                        let places = i32::try_from(places).map_err(|_| {
                            SQLError::TypeMismatch(format!("round scale out of range: {places}"))
                        })?;
                        return to_decimal(&args[0])?
                            .round_to_scale(places)
                            .map(Value::Decimal)
                            .ok_or_else(|| {
                                SQLError::TypeMismatch("decimal round overflow".into())
                            });
                    }
                    let v = to_f64(&args[0])?;
                    let places =
                        i32::try_from(to_i64(&args[1])?).map_err(|_| out_of_range("integer"))?;
                    let scale = 10f64.powi(places);
                    Ok(Value::Float((v * scale).round() / scale))
                }
                _ => Err(SQLError::TypeMismatch("round takes 1-2 args".into())),
            },
            "ceil" | "ceiling" => match &args[0] {
                Value::Int(i) => Ok(Value::Int(*i)),
                Value::Float(f) => Ok(Value::Float(f.ceil())),
                Value::Decimal(d) => Ok(Value::Decimal(d.ceil())),
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!("ceil({other:?})"))),
            },
            "floor" => match &args[0] {
                Value::Int(i) => Ok(Value::Int(*i)),
                Value::Float(f) => Ok(Value::Float(f.floor())),
                Value::Decimal(d) => Ok(Value::Decimal(d.floor())),
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!("floor({other:?})"))),
            },
            "power" | "pow" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("power takes 2 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Float(to_f64(&args[0])?.powf(to_f64(&args[1])?)))
            }
            "sqrt" => float1(args, "sqrt", f64::sqrt),
            "mod" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("mod takes 2 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                match (&args[0], &args[1]) {
                    (Value::Int(_), Value::Int(0)) => Err(division_by_zero()),
                    (Value::Int(a), Value::Int(b)) => a
                        .checked_rem(*b)
                        .map(Value::Int)
                        .ok_or_else(|| out_of_range("bigint")),
                    (a, b) if matches!(a, Value::Decimal(_)) || matches!(b, Value::Decimal(_)) => {
                        let divisor = to_decimal(b)?;
                        if divisor.is_zero() {
                            Err(division_by_zero())
                        } else {
                            to_decimal(a)?
                                .checked_rem(&divisor)
                                .map(Value::Decimal)
                                .ok_or_else(|| out_of_range("numeric"))
                        }
                    }
                    (a, b) => {
                        let af = to_f64(a)?;
                        let bf = to_f64(b)?;
                        if bf == 0.0 {
                            Err(division_by_zero())
                        } else {
                            Ok(Value::Float(af % bf))
                        }
                    }
                }
            }
            "div" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("div takes 2 args".into()));
                }
                let divisor = to_i64(&args[1])?;
                if divisor == 0 {
                    return Err(division_by_zero());
                }
                let dividend = to_i64(&args[0])?;
                dividend
                    .checked_div(divisor)
                    .map(Value::Int)
                    .ok_or_else(|| out_of_range("bigint"))
            }
            "gcd" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("gcd takes 2 args".into()));
                }
                Ok(Value::Int(gcd_i64(to_i64(&args[0])?, to_i64(&args[1])?)?))
            }
            "lcm" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("lcm takes 2 args".into()));
                }
                let a = to_i64(&args[0])?;
                let b = to_i64(&args[1])?;
                if a == 0 || b == 0 {
                    Ok(Value::Int(0))
                } else {
                    let gcd = i128::from(gcd_i64(a, b)?);
                    let value = (i128::from(a) / gcd)
                        .checked_mul(i128::from(b))
                        .and_then(i128::checked_abs)
                        .and_then(|value| i64::try_from(value).ok())
                        .ok_or_else(|| out_of_range("bigint"))?;
                    Ok(Value::Int(value))
                }
            }
            "starts_with" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("starts_with takes 2 args".into()));
                }
                Ok(Value::Bool(
                    value_to_string(&args[0]).starts_with(&value_to_string(&args[1])),
                ))
            }
            "position" | "strpos" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("position takes 2 args".into()));
                }
                let haystack = value_to_string(&args[0]);
                let needle = value_to_string(&args[1]);
                if needle.is_empty() {
                    return Ok(Value::Int(0));
                }
                let idx = haystack
                    .find(&needle)
                    .map_or(0, |b| haystack[..b].chars().count() as i64 + 1);
                Ok(Value::Int(idx))
            }
            "ascii" => {
                let s = value_to_string(&args[0]);
                Ok(Value::Int(s.chars().next().map(|c| c as i64).unwrap_or(0)))
            }
            "like" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("LIKE takes 2 args".into()));
                }
                if args.iter().any(|argument| matches!(argument, Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Bool(like_match(
                    &value_to_string(&args[0]),
                    &value_to_string(&args[1]),
                    false,
                )))
            }
            "ilike" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("ILIKE takes 2 args".into()));
                }
                if args.iter().any(|argument| matches!(argument, Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Bool(like_match(
                    &value_to_string(&args[0]),
                    &value_to_string(&args[1]),
                    true,
                )))
            }
            "chr" => {
                let n = to_i64(&args[0])?;
                let code_point = u32::try_from(n)
                    .map_err(|_| SQLError::TypeMismatch(format!("chr: invalid code point {n}")))?;
                let c = char::from_u32(code_point).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("chr: invalid code point {n}"))
                })?;
                Ok(Value::Str(c.to_string()))
            }
            "regexp_match" | "regexp_matches" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch(
                        "regexp_match takes 2 or 3 args".into(),
                    ));
                }
                let s = value_to_string(&args[0]);
                let pat = value_to_string(&args[1]);
                let case_insensitive = args
                    .get(2)
                    .map(|v| value_to_string(v).contains('i'))
                    .unwrap_or(false);
                let pat = if case_insensitive {
                    format!("(?i){pat}")
                } else {
                    pat
                };
                let re = regex::Regex::new(&pat)
                    .map_err(|e| SQLError::TypeMismatch(format!("regex: {e}")))?;
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
                                SQLError::Internal(
                                    "regex capture set omitted its mandatory full match".into(),
                                )
                            })?;
                            Ok(Value::List(vec![Value::Str(full_match.as_str().into())]))
                        } else {
                            Ok(Value::List(groups))
                        }
                    }
                }
            }
            "regexp_replace" => {
                if args.len() < 3 {
                    return Err(SQLError::TypeMismatch(
                        "regexp_replace takes 3 or 4 args".into(),
                    ));
                }
                let s = value_to_string(&args[0]);
                let pat = value_to_string(&args[1]);
                let repl = value_to_string(&args[2]);
                let flags = args.get(3).map(|v| value_to_string(v)).unwrap_or_default();
                let global = flags.contains('g');
                let pat = if flags.contains('i') {
                    format!("(?i){pat}")
                } else {
                    pat
                };
                let re = regex::Regex::new(&pat)
                    .map_err(|e| SQLError::TypeMismatch(format!("regex: {e}")))?;
                let out = if global {
                    re.replace_all(&s, repl.as_str()).into_owned()
                } else {
                    re.replace(&s, repl.as_str()).into_owned()
                };
                Ok(Value::Str(out))
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })())
}

#[cfg(test)]
mod tests {
    use super::eval_core_functions;
    use uqa_core::Value;

    #[test]
    fn like_and_ilike_propagate_null_arguments() {
        for name in ["like", "ilike"] {
            for arguments in [
                vec![Value::Null, Value::Str("%".into())],
                vec![Value::Str("text".into()), Value::Null],
            ] {
                assert_eq!(
                    eval_core_functions(name, &arguments).unwrap().unwrap(),
                    Value::Null
                );
            }
        }
    }
}
