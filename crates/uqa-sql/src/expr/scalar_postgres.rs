//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Extended `PostgreSQL` scalar and lowered operator built-ins.

use super::{
    allocation_error, compile_pg_regex, eval_between, eval_comparison_op, expect_str,
    nonnegative_usize, out_of_range, quote_ident, quote_literal, similar_to_regex, to_i64,
    value_to_string, values_equal, BinaryOp, DecimalValue, Result, SQLError, Value,
};

pub(super) fn eval_postgres_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    const NAMES: &[&str] = &[
        "factorial",
        "bit_length",
        "to_hex",
        "string_to_array",
        "string_to_table",
        "quote_ident",
        "quote_literal",
        "quote_nullable",
        "regexp_count",
        "regexp_like",
        "similar_to",
        "num_nulls",
        "num_nonnulls",
        "current_database",
        "current_catalog",
        "current_user",
        "session_user",
        "array_positions",
        "array_replace",
        "array_to_string",
        "array_fill",
        "trim_array",
        "array_sample",
        "array_overlap",
        "__subscript",
        "__slice",
        "__any_op",
        "__all_op",
        "__is_distinct",
        "__between_symmetric",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Value> {
        match name {
            // -------------------------------------------------------------
            // PostgreSQL scalar surface: math, strings, arrays, operators
            // lowered to internal functions.
            // -------------------------------------------------------------
            "factorial" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("factorial takes 1 arg".into()));
                }
                if matches!(args[0], Value::Null) {
                    return Ok(Value::Null);
                }
                let n = to_i64(&args[0])?;
                if n < 0 {
                    return Err(SQLError::Routine {
                        sqlstate: "2201F".into(),
                        message: "factorial of a negative number is undefined".into(),
                    });
                }
                let mut acc: i128 = 1;
                for k in 2..=n as i128 {
                    acc = acc.checked_mul(k).ok_or_else(|| out_of_range("numeric"))?;
                }
                if let Ok(small) = i64::try_from(acc) {
                    return Ok(Value::Int(small));
                }
                DecimalValue::parse(&acc.to_string())
                    .map(Value::Decimal)
                    .ok_or_else(|| out_of_range("numeric"))
            }
            "bit_length" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                match args.first() {
                    Some(Value::Bytes(b)) => Ok(Value::Int(b.len() as i64 * 8)),
                    Some(other) => Ok(Value::Int(value_to_string(other).len() as i64 * 8)),
                    None => Err(SQLError::TypeMismatch("bit_length takes 1 arg".into())),
                }
            }
            "to_hex" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                let n = to_i64(&args[0])?;
                // int4 arguments format as 32-bit two's complement
                // (`to_hex(-1)` = 'ffffffff'), wider values as 64-bit.
                if let Ok(small) = i32::try_from(n) {
                    Ok(Value::Str(format!("{:x}", small as u32)))
                } else {
                    Ok(Value::Str(format!("{:x}", n as u64)))
                }
            }
            "string_to_array" | "string_to_table" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch(
                        "string_to_array takes 2-3 args".into(),
                    ));
                }
                if matches!(args[0], Value::Null) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let null_marker = args.get(2).filter(|v| !matches!(v, Value::Null));
                let mark = |part: &str| -> Value {
                    if let Some(marker) = null_marker {
                        if part == value_to_string(marker) {
                            return Value::Null;
                        }
                    }
                    Value::Str(part.to_string())
                };
                let items: Vec<Value> = match &args[1] {
                    // NULL separator: split into individual characters.
                    Value::Null => s.chars().map(|c| mark(&c.to_string())).collect(),
                    sep => {
                        let sep = value_to_string(sep);
                        if s.is_empty() {
                            Vec::new()
                        } else if sep.is_empty() {
                            vec![mark(&s)]
                        } else {
                            s.split(sep.as_str()).map(mark).collect()
                        }
                    }
                };
                Ok(Value::List(items))
            }
            "quote_ident" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Str(quote_ident(&expect_str(args, 0)?)))
            }
            "quote_literal" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Str(quote_literal(&expect_str(args, 0)?)))
            }
            "quote_nullable" => match args.first() {
                Some(Value::Null) | None => Ok(Value::Str("NULL".into())),
                Some(other) => Ok(Value::Str(quote_literal(&value_to_string(other)))),
            },
            "regexp_count" => {
                if args.len() < 2 || args.len() > 4 {
                    return Err(SQLError::TypeMismatch("regexp_count takes 2-4 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let pat = value_to_string(&args[1]);
                let start =
                    usize::try_from(args.get(2).map(to_i64).transpose()?.unwrap_or(1).max(1))
                        .unwrap_or(usize::MAX);
                let flags = args.get(3).map(value_to_string).unwrap_or_default();
                let re = compile_pg_regex(&pat, &flags)?;
                let chars: Vec<char> = s.chars().collect();
                let tail: String = chars[(start - 1).min(chars.len())..].iter().collect();
                Ok(Value::Int(re.find_iter(&tail).count() as i64))
            }
            "regexp_like" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch("regexp_like takes 2-3 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let pat = value_to_string(&args[1]);
                let flags = args.get(2).map(value_to_string).unwrap_or_default();
                let re = compile_pg_regex(&pat, &flags)?;
                Ok(Value::Bool(re.is_match(&s)))
            }
            "similar_to" => {
                // SIMILAR TO: SQL regex anchored over the whole string.
                if args.len() < 2 {
                    return Err(SQLError::TypeMismatch("similar_to takes 2 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let pat = similar_to_regex(&value_to_string(&args[1]));
                let re = regex::Regex::new(&pat)
                    .map_err(|e| SQLError::TypeMismatch(format!("SIMILAR TO pattern: {e}")))?;
                Ok(Value::Bool(re.is_match(&s)))
            }
            "num_nulls" => Ok(Value::Int(
                args.iter().filter(|v| matches!(v, Value::Null)).count() as i64,
            )),
            "num_nonnulls" => Ok(Value::Int(
                args.iter().filter(|v| !matches!(v, Value::Null)).count() as i64,
            )),
            // The engine has one database and one logical user identity; schema
            // identifiers are intercepted above because they are session-scoped.
            "current_database" | "current_catalog" => Ok(Value::Str("uqa".into())),
            "current_user" | "session_user" => Ok(Value::Str("uqa".into())),
            "array_positions" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(
                        "array_positions takes 2 args".into(),
                    ));
                }
                match &args[0] {
                    Value::List(items) => Ok(Value::List(
                        items
                            .iter()
                            .enumerate()
                            .filter(|(_, v)| *v == &args[1])
                            .map(|(i, _)| Value::Int((i + 1) as i64))
                            .collect(),
                    )),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "array_positions: not an array {other:?}"
                    ))),
                }
            }
            "array_replace" => {
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("array_replace takes 3 args".into()));
                }
                match &args[0] {
                    Value::List(items) => Ok(Value::List(
                        items
                            .iter()
                            .map(|v| {
                                if *v == args[1] {
                                    args[2].clone()
                                } else {
                                    v.clone()
                                }
                            })
                            .collect(),
                    )),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "array_replace: not an array {other:?}"
                    ))),
                }
            }
            "array_to_string" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch(
                        "array_to_string takes 2-3 args".into(),
                    ));
                }
                let Value::List(items) = &args[0] else {
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch(format!(
                        "array_to_string: not an array {:?}",
                        args[0]
                    )));
                };
                if matches!(args[1], Value::Null) {
                    return Ok(Value::Null);
                }
                let sep = value_to_string(&args[1]);
                let null_text = args.get(2).filter(|v| !matches!(v, Value::Null));
                let mut parts: Vec<String> = Vec::with_capacity(items.len());
                for item in items {
                    if matches!(item, Value::Null) {
                        if let Some(marker) = null_text {
                            parts.push(value_to_string(marker));
                        }
                        continue;
                    }
                    parts.push(value_to_string(item));
                }
                Ok(Value::Str(parts.join(&sep)))
            }
            "array_fill" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_fill takes 2 args".into()));
                }
                let Value::List(dims) = &args[1] else {
                    return Err(SQLError::TypeMismatch(
                        "array_fill: dimensions must be an integer array".into(),
                    ));
                };
                if dims.len() != 1 {
                    return Err(SQLError::Unsupported(
                        "array_fill supports one dimension".into(),
                    ));
                }
                let n = nonnegative_usize(to_i64(&dims[0])?.max(0), "array_fill dimension")?;
                let mut values = Vec::new();
                values
                    .try_reserve_exact(n)
                    .map_err(|_| allocation_error("array_fill"))?;
                values.resize(n, args[0].clone());
                Ok(Value::List(values))
            }
            "trim_array" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("trim_array takes 2 args".into()));
                }
                let Value::List(items) = &args[0] else {
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch("trim_array: not an array".into()));
                };
                let n = to_i64(&args[1])?;
                let n = usize::try_from(n).ok();
                if n.is_none_or(|n| n > items.len()) {
                    return Err(SQLError::Routine {
                        sqlstate: "2202E".into(),
                        message: format!(
                            "number of elements to trim must be between 0 and {}",
                            items.len()
                        ),
                    });
                }
                let n = n.ok_or_else(|| out_of_range("array trim count"))?;
                Ok(Value::List(items[..items.len() - n].to_vec()))
            }
            "array_sample" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_sample takes 2 args".into()));
                }
                let Value::List(items) = &args[0] else {
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch("array_sample: not an array".into()));
                };
                let n = to_i64(&args[1])?;
                let n = usize::try_from(n).ok();
                if n.is_none_or(|n| n > items.len()) {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: format!("sample size must be between 0 and {}", items.len()),
                    });
                }
                let n = n.ok_or_else(|| out_of_range("array sample size"))?;
                let mut pool = items.clone();
                let mut out = Vec::with_capacity(n);
                let mut seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as u64 | 1)
                    .unwrap_or(1);
                for _ in 0..n {
                    seed = seed
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    let idx = (seed >> 33) as usize % pool.len();
                    out.push(pool.swap_remove(idx));
                }
                Ok(Value::List(out))
            }
            "array_overlap" => {
                // `&&` operator: true when the arrays share any non-null
                // element.
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array overlap takes 2 args".into()));
                }
                match (&args[0], &args[1]) {
                    (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                    (Value::List(a), Value::List(b)) => Ok(Value::Bool(a.iter().any(|x| {
                        !matches!(x, Value::Null) && b.iter().any(|y| values_equal(x, y))
                    }))),
                    _ => Err(SQLError::TypeMismatch(
                        "array overlap: both args must be arrays".into(),
                    )),
                }
            }
            "__subscript" => {
                // 1-based array subscripting; out-of-range yields NULL.
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("subscript takes 2 args".into()));
                }
                match (&args[0], &args[1]) {
                    (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                    (Value::List(items), idx) => {
                        let idx = to_i64(idx)?;
                        let Ok(idx) = usize::try_from(idx) else {
                            return Ok(Value::Null);
                        };
                        if idx < 1 || idx > items.len() {
                            return Ok(Value::Null);
                        }
                        Ok(items[idx - 1].clone())
                    }
                    (Value::Map(map), key) => Ok(map
                        .get(&value_to_string(key))
                        .cloned()
                        .unwrap_or(Value::Null)),
                    (other, _) => Err(SQLError::TypeMismatch(format!(
                        "cannot subscript {other:?}"
                    ))),
                }
            }
            "__slice" => {
                // Array slice `arr[lo:hi]`; open bounds arrive as NULL and
                // clamp to the array, PostgreSQL-style.
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("slice takes 3 args".into()));
                }
                match &args[0] {
                    Value::Null => Ok(Value::Null),
                    Value::List(items) => {
                        let lo = match &args[1] {
                            Value::Null => 1,
                            other => to_i64(other)?,
                        }
                        .max(1);
                        let hi = match &args[2] {
                            Value::Null => items.len() as i64,
                            other => to_i64(other)?,
                        }
                        .min(items.len() as i64);
                        if hi < lo || lo > items.len() as i64 {
                            return Ok(Value::List(Vec::new()));
                        }
                        let lo =
                            usize::try_from(lo - 1).map_err(|_| out_of_range("array slice"))?;
                        let hi = usize::try_from(hi).map_err(|_| out_of_range("array slice"))?;
                        Ok(Value::List(items[lo..hi].to_vec()))
                    }
                    other => Err(SQLError::TypeMismatch(format!("cannot slice {other:?}"))),
                }
            }
            "__any_op" | "__all_op" => {
                // `expr op ANY(array)` / `expr op ALL(array)` with Kleene
                // aggregation over the element comparisons.
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("ANY/ALL takes 3 args".into()));
                }
                let op = match value_to_string(&args[2]).as_str() {
                    "=" => BinaryOp::Equal,
                    "<>" | "!=" => BinaryOp::NotEqual,
                    "<" => BinaryOp::Less,
                    "<=" => BinaryOp::LessEqual,
                    ">" => BinaryOp::Greater,
                    ">=" => BinaryOp::GreaterEqual,
                    other => {
                        return Err(SQLError::Unsupported(format!(
                            "operator `{other}` with ANY/ALL"
                        )));
                    }
                };
                let Value::List(items) = &args[1] else {
                    if matches!(args[1], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch("ANY/ALL requires an array".into()));
                };
                let is_any = name == "__any_op";
                let mut saw_null = false;
                for item in items {
                    match eval_comparison_op(op, &args[0], item)? {
                        Value::Bool(true) if is_any => return Ok(Value::Bool(true)),
                        Value::Bool(false) if !is_any => return Ok(Value::Bool(false)),
                        Value::Null => saw_null = true,
                        _ => {}
                    }
                }
                if saw_null {
                    return Ok(Value::Null);
                }
                Ok(Value::Bool(!is_any))
            }
            "__is_distinct" => {
                // IS DISTINCT FROM: null-safe inequality (never NULL).
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(
                        "IS DISTINCT FROM takes 2 args".into(),
                    ));
                }
                let distinct = match (&args[0], &args[1]) {
                    (Value::Null, Value::Null) => false,
                    (Value::Null, _) | (_, Value::Null) => true,
                    (a, b) => !values_equal(a, b),
                };
                Ok(Value::Bool(distinct))
            }
            "__between_symmetric" => {
                // BETWEEN SYMMETRIC: PostgreSQL rewrites to
                // `(a >= x AND a <= y) OR (a >= y AND a <= x)` and the
                // three-valued OR of the two window tests.
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch(
                        "BETWEEN SYMMETRIC takes 3 args".into(),
                    ));
                }
                let forward = eval_between(&args[0], &args[1], &args[2])?;
                let backward = eval_between(&args[0], &args[2], &args[1])?;
                Ok(match (&forward, &backward) {
                    (Value::Bool(true), _) | (_, Value::Bool(true)) => Value::Bool(true),
                    (Value::Null, _) | (_, Value::Null) => Value::Null,
                    _ => Value::Bool(false),
                })
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })())
}
