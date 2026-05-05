//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar expression evaluator: turns an [`Expr`] into a [`Value`] under
//! a row context (column -> value) and a parameter binding.

use uqa_core::Value;

use crate::ast::{BinaryOp, Expr};
use crate::error::{Result, SqlError};
use crate::params::SqlParam;
use crate::result::ResultRow;

pub struct EvalContext<'a> {
    pub row: Option<&'a ResultRow>,
    pub params: &'a [SqlParam],
}

impl<'a> EvalContext<'a> {
    pub fn new(row: Option<&'a ResultRow>, params: &'a [SqlParam]) -> Self {
        Self { row, params }
    }
}

/// Evaluate a value-producing expression. Function calls are *not*
/// dispatched here; the compiler routes them through the function
/// registry instead. Calling `eval` on a `Func` expr returns
/// `Unsupported` so latent function-in-projection bugs surface loudly.
pub fn eval(expr: &Expr, ctx: &EvalContext<'_>) -> Result<Value> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Param(i) => match ctx.params.get(i.saturating_sub(1)) {
            Some(SqlParam::Scalar(v)) => Ok(v.clone()),
            Some(SqlParam::Vector(v)) => Ok(Value::List(
                v.iter().map(|x| Value::Float(f64::from(*x))).collect(),
            )),
            None => Err(SqlError::MissingParam(*i)),
        },
        Expr::Column(name) => {
            let row = ctx
                .row
                .ok_or_else(|| SqlError::Internal("column reference without row context".into()))?;
            // Plain column refs match either an unqualified key or the
            // suffix of a qualified `table.col` key, so the same row
            // shape works for single-table SELECTs and JOIN tuples.
            if let Some(v) = row.get(name) {
                return Ok(v.clone());
            }
            let suffix = format!(".{name}");
            for (key, value) in row {
                if key.ends_with(&suffix) {
                    return Ok(value.clone());
                }
            }
            Ok(Value::Null)
        }
        Expr::QualifiedColumn { qualifier, column } => {
            let row = ctx
                .row
                .ok_or_else(|| SqlError::Internal("column reference without row context".into()))?;
            let key = format!("{qualifier}.{column}");
            Ok(row.get(&key).cloned().unwrap_or(Value::Null))
        }
        Expr::Array(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for e in elements {
                out.push(eval(e, ctx)?);
            }
            Ok(Value::List(out))
        }
        Expr::Star => Err(SqlError::Internal("`*` cannot be evaluated".into())),
        Expr::Func { name, args } => {
            // Functions registered in the operator registry (text_match,
            // knn_match, ...) are dispatched by the engine; only pure
            // scalar built-ins are evaluated inline here.
            let lower = name.to_ascii_lowercase();
            if crate::registry::is_registered(&lower) {
                return Err(SqlError::Unsupported(format!(
                    "scalar evaluation of `{name}` is not supported (use the function registry)"
                )));
            }
            let evaluated: Vec<Value> = args
                .iter()
                .map(|a| eval(a, ctx))
                .collect::<Result<Vec<_>>>()?;
            eval_scalar_function(&lower, &evaluated)
        }
        Expr::WindowCall { name, .. } => Err(SqlError::Unsupported(format!(
            "window function `{name}` must be evaluated by the window-aware executor"
        ))),
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let base_value = match base {
                Some(b) => Some(eval(b, ctx)?),
                None => None,
            };
            for (cond, result) in when {
                let matched = match &base_value {
                    Some(bv) => values_equal(bv, &eval(cond, ctx)?),
                    None => truthy(&eval(cond, ctx)?),
                };
                if matched {
                    return eval(result, ctx);
                }
            }
            match else_branch {
                Some(e) => eval(e, ctx),
                None => Ok(Value::Null),
            }
        }
        Expr::Cast { expr, ty } => {
            let v = eval(expr, ctx)?;
            cast_value(&v, ty)
        }
        Expr::Binary { op, lhs, rhs } => eval_binary(*op, lhs, rhs, ctx),
        Expr::Not(inner) => {
            let v = eval(inner, ctx)?;
            Ok(Value::Bool(!truthy(&v)))
        }
        Expr::And(items) => {
            for item in items {
                let v = eval(item, ctx)?;
                if !truthy(&v) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        Expr::Or(items) => {
            for item in items {
                let v = eval(item, ctx)?;
                if truthy(&v) {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        Expr::IsNull { expr, negated } => {
            let v = eval(expr, ctx)?;
            let is_null = matches!(v, Value::Null);
            Ok(Value::Bool(if *negated { !is_null } else { is_null }))
        }
        Expr::Between { expr, low, high } => {
            let v = eval(expr, ctx)?;
            let lo = eval(low, ctx)?;
            let hi = eval(high, ctx)?;
            let ge = compare(&v, &lo)?.is_ge();
            let le = compare(&v, &hi)?.is_le();
            Ok(Value::Bool(ge && le))
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let v = eval(expr, ctx)?;
            for item in list {
                let candidate = eval(item, ctx)?;
                if values_equal(&v, &candidate) {
                    return Ok(Value::Bool(!*negated));
                }
            }
            Ok(Value::Bool(*negated))
        }
    }
}

fn eval_binary(op: BinaryOp, lhs: &Expr, rhs: &Expr, ctx: &EvalContext<'_>) -> Result<Value> {
    let l = eval(lhs, ctx)?;
    let r = eval(rhs, ctx)?;
    match op {
        BinaryOp::Equal => Ok(Value::Bool(values_equal(&l, &r))),
        BinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&l, &r))),
        BinaryOp::Less => Ok(Value::Bool(compare(&l, &r)?.is_lt())),
        BinaryOp::LessEqual => Ok(Value::Bool(compare(&l, &r)?.is_le())),
        BinaryOp::Greater => Ok(Value::Bool(compare(&l, &r)?.is_gt())),
        BinaryOp::GreaterEqual => Ok(Value::Bool(compare(&l, &r)?.is_ge())),
        BinaryOp::Add => arith(&l, &r, op),
        BinaryOp::Subtract => arith(&l, &r, op),
        BinaryOp::Multiply => arith(&l, &r, op),
        BinaryOp::Divide => arith(&l, &r, op),
    }
}

/// `NULL` is falsy; otherwise truthy iff the value coerces to a non-zero
/// boolean / number / non-empty string.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        _ => true,
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        _ => a == b,
    }
}

fn compare(a: &Value, b: &Value) -> Result<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => Ok(Ordering::Equal),
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => Ok(x.partial_cmp(y).unwrap_or(Ordering::Equal)),
        (Value::Int(x), Value::Float(y)) => {
            Ok((*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal))
        }
        (Value::Float(x), Value::Int(y)) => {
            Ok(x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal))
        }
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        (lhs, rhs) => Err(SqlError::TypeMismatch(format!(
            "cannot compare {lhs:?} with {rhs:?}"
        ))),
    }
}

fn arith(a: &Value, b: &Value, op: BinaryOp) -> Result<Value> {
    let lf = to_f64(a)?;
    let rf = to_f64(b)?;
    let result = match op {
        BinaryOp::Add => lf + rf,
        BinaryOp::Subtract => lf - rf,
        BinaryOp::Multiply => lf * rf,
        BinaryOp::Divide => {
            if rf == 0.0 {
                return Err(SqlError::TypeMismatch("division by zero".into()));
            }
            lf / rf
        }
        _ => unreachable!("non-arith op routed through arith"),
    };
    // Preserve integer-ness when both operands were ints and the result
    // is whole.
    if matches!((a, b), (Value::Int(_), Value::Int(_))) && result.fract() == 0.0 {
        Ok(Value::Int(result as i64))
    } else {
        Ok(Value::Float(result))
    }
}

// -------------------------------------------------------------------------
// Built-in scalar functions
// -------------------------------------------------------------------------

/// Dispatch table for built-in scalar SQL functions. Mirrors
/// `_call_scalar_function` in `uqa/sql/expr_evaluator.py`. Function
/// names are lower-cased before lookup.
fn eval_scalar_function(name: &str, args: &[Value]) -> Result<Value> {
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
                return Err(SqlError::TypeMismatch("nullif takes 2 args".into()));
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
            let s = expect_str(args, 0)?;
            Ok(Value::Int(s.chars().count() as i64))
        }
        "octet_length" => {
            let s = expect_str(args, 0)?;
            Ok(Value::Int(s.len() as i64))
        }
        "trim" => string1(args, |s| s.trim().to_string()),
        "ltrim" => string1(args, |s| s.trim_start().to_string()),
        "rtrim" => string1(args, |s| s.trim_end().to_string()),
        "initcap" => string1(args, initcap_str),
        "reverse" => string1(args, |s| s.chars().rev().collect()),
        "concat" => {
            let mut buf = String::new();
            for a in args {
                if matches!(a, Value::Null) {
                    continue;
                }
                buf.push_str(&value_to_string(a));
            }
            Ok(Value::Str(buf))
        }
        "concat_ws" => {
            if args.is_empty() {
                return Err(SqlError::TypeMismatch("concat_ws needs separator".into()));
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
                return Err(SqlError::TypeMismatch("replace takes 3 args".into()));
            }
            let s = value_to_string(&args[0]);
            let from = value_to_string(&args[1]);
            let to = value_to_string(&args[2]);
            Ok(Value::Str(s.replace(&from, &to)))
        }
        "substring" | "substr" => {
            // SUBSTRING(string, start [, length]). 1-indexed per SQL.
            if args.len() < 2 || args.len() > 3 {
                return Err(SqlError::TypeMismatch("substring takes 2-3 args".into()));
            }
            let s = value_to_string(&args[0]);
            let start = to_i64(&args[1])?;
            let chars: Vec<char> = s.chars().collect();
            let n = chars.len() as i64;
            let begin = (start.max(1) - 1).min(n);
            let take = if args.len() == 3 {
                to_i64(&args[2])?.max(0)
            } else {
                n - begin
            };
            let end = (begin + take).min(n);
            let slice: String = chars[(begin as usize)..(end as usize)].iter().collect();
            Ok(Value::Str(slice))
        }
        "left" => {
            if args.len() != 2 {
                return Err(SqlError::TypeMismatch("left takes 2 args".into()));
            }
            let s = value_to_string(&args[0]);
            let n = to_i64(&args[1])?;
            let chars: Vec<char> = s.chars().collect();
            let take = n.clamp(0, chars.len() as i64) as usize;
            Ok(Value::Str(chars[..take].iter().collect()))
        }
        "right" => {
            if args.len() != 2 {
                return Err(SqlError::TypeMismatch("right takes 2 args".into()));
            }
            let s = value_to_string(&args[0]);
            let n = to_i64(&args[1])?;
            let chars: Vec<char> = s.chars().collect();
            let take = n.clamp(0, chars.len() as i64) as usize;
            let start = chars.len() - take;
            Ok(Value::Str(chars[start..].iter().collect()))
        }
        "abs" => match &args[0] {
            Value::Int(i) => Ok(Value::Int(i.abs())),
            Value::Float(f) => Ok(Value::Float(f.abs())),
            Value::Null => Ok(Value::Null),
            other => Err(SqlError::TypeMismatch(format!(
                "abs() expected number, got {other:?}"
            ))),
        },
        "round" => match args.len() {
            1 => match &args[0] {
                Value::Int(i) => Ok(Value::Int(*i)),
                Value::Float(f) => Ok(Value::Float(f.round())),
                Value::Null => Ok(Value::Null),
                other => Err(SqlError::TypeMismatch(format!("round({other:?})"))),
            },
            2 => {
                let v = to_f64(&args[0])?;
                let places = to_i64(&args[1])?;
                let scale = 10f64.powi(places as i32);
                Ok(Value::Float((v * scale).round() / scale))
            }
            _ => Err(SqlError::TypeMismatch("round takes 1-2 args".into())),
        },
        "ceil" | "ceiling" => match &args[0] {
            Value::Int(i) => Ok(Value::Int(*i)),
            Value::Float(f) => Ok(Value::Float(f.ceil())),
            Value::Null => Ok(Value::Null),
            other => Err(SqlError::TypeMismatch(format!("ceil({other:?})"))),
        },
        "floor" => match &args[0] {
            Value::Int(i) => Ok(Value::Int(*i)),
            Value::Float(f) => Ok(Value::Float(f.floor())),
            Value::Null => Ok(Value::Null),
            other => Err(SqlError::TypeMismatch(format!("floor({other:?})"))),
        },
        "power" | "pow" => {
            if args.len() != 2 {
                return Err(SqlError::TypeMismatch("power takes 2 args".into()));
            }
            Ok(Value::Float(to_f64(&args[0])?.powf(to_f64(&args[1])?)))
        }
        "sqrt" => Ok(Value::Float(to_f64(&args[0])?.sqrt())),
        "mod" => {
            if args.len() != 2 {
                return Err(SqlError::TypeMismatch("mod takes 2 args".into()));
            }
            match (&args[0], &args[1]) {
                (Value::Int(a), Value::Int(b)) if *b != 0 => Ok(Value::Int(a % b)),
                (a, b) => {
                    let af = to_f64(a)?;
                    let bf = to_f64(b)?;
                    if bf == 0.0 {
                        Err(SqlError::TypeMismatch("modulo by zero".into()))
                    } else {
                        Ok(Value::Float(af % bf))
                    }
                }
            }
        }
        "starts_with" => {
            if args.len() != 2 {
                return Err(SqlError::TypeMismatch("starts_with takes 2 args".into()));
            }
            Ok(Value::Bool(
                value_to_string(&args[0]).starts_with(&value_to_string(&args[1])),
            ))
        }
        "position" | "strpos" => {
            if args.len() != 2 {
                return Err(SqlError::TypeMismatch("position takes 2 args".into()));
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
                return Err(SqlError::TypeMismatch("LIKE takes 2 args".into()));
            }
            Ok(Value::Bool(like_match(
                &value_to_string(&args[0]),
                &value_to_string(&args[1]),
                false,
            )))
        }
        "ilike" => {
            if args.len() != 2 {
                return Err(SqlError::TypeMismatch("ILIKE takes 2 args".into()));
            }
            Ok(Value::Bool(like_match(
                &value_to_string(&args[0]),
                &value_to_string(&args[1]),
                true,
            )))
        }
        "chr" => {
            let n = to_i64(&args[0])?;
            let c = char::from_u32(n as u32)
                .ok_or_else(|| SqlError::TypeMismatch(format!("chr: invalid code point {n}")))?;
            Ok(Value::Str(c.to_string()))
        }
        other => Err(SqlError::Unsupported(format!("scalar function `{other}`"))),
    }
}

/// SQL `LIKE` / `ILIKE` matching: `%` is any run of characters,
/// `_` is exactly one character; everything else is literal. Uses a
/// linear backtracking matcher tuned for single-pattern queries.
fn like_match(haystack: &str, pattern: &str, case_insensitive: bool) -> bool {
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

fn cast_value(v: &Value, ty: &str) -> Result<Value> {
    if matches!(v, Value::Null) {
        return Ok(Value::Null);
    }
    match ty {
        "integer" | "int" | "int2" | "int4" | "int8" | "bigint" | "smallint" | "serial"
        | "bigserial" | "serial4" | "serial8" | "pg_catalog.int4" | "pg_catalog.int8"
        | "pg_catalog.int2" => Ok(Value::Int(to_i64(v)?)),
        "real" | "float4" | "float8" | "double" | "double precision" | "numeric" | "decimal" => {
            Ok(Value::Float(to_f64(v)?))
        }
        "text" | "varchar" | "character" | "char" | "bpchar" | "name" | "uuid" => {
            Ok(Value::Str(value_to_string(v)))
        }
        "boolean" | "bool" => Ok(Value::Bool(truthy(v))),
        other => Err(SqlError::Unsupported(format!("CAST AS {other}"))),
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => "".into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.clone(),
        Value::Bool(b) => (if *b { "true" } else { "false" }).into(),
        other => format!("{other:?}"),
    }
}

fn expect_str(args: &[Value], idx: usize) -> Result<String> {
    args.get(idx)
        .map(value_to_string)
        .ok_or_else(|| SqlError::TypeMismatch(format!("missing arg #{idx}")))
}

fn string1<F: FnOnce(&str) -> String>(args: &[Value], f: F) -> Result<Value> {
    if args.is_empty() {
        return Err(SqlError::TypeMismatch("string fn needs 1 arg".into()));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let s = value_to_string(&args[0]);
    Ok(Value::Str(f(&s)))
}

fn initcap_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for ch in s.chars() {
        if ch.is_whitespace() {
            out.push(ch);
            start = true;
            continue;
        }
        if start {
            for c in ch.to_uppercase() {
                out.push(c);
            }
            start = false;
        } else {
            for c in ch.to_lowercase() {
                out.push(c);
            }
        }
    }
    out
}

fn to_i64(v: &Value) -> Result<i64> {
    match v {
        Value::Int(n) => Ok(*n),
        Value::Float(f) => Ok(*f as i64),
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Str(s) => s
            .parse()
            .map_err(|_| SqlError::TypeMismatch(format!("cannot parse {s:?} as integer"))),
        other => Err(SqlError::TypeMismatch(format!(
            "expected integer, got {other:?}"
        ))),
    }
}

fn to_f64(v: &Value) -> Result<f64> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(SqlError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

/// Coerce a [`Value`] into a `Vec<f32>` if it is a homogeneous numeric
/// list (used to read vector literals from `ARRAY[...]` or `$N` Vector
/// params).
pub fn value_to_vector(v: &Value) -> Result<Vec<f32>> {
    match v {
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let x = match item {
                    Value::Float(f) => *f as f32,
                    Value::Int(i) => *i as f32,
                    other => {
                        return Err(SqlError::TypeMismatch(format!(
                            "vector element must be numeric, got {other:?}"
                        )))
                    }
                };
                out.push(x);
            }
            Ok(out)
        }
        other => Err(SqlError::TypeMismatch(format!(
            "expected vector (numeric list), got {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Expr;

    #[test]
    fn literal_passthrough() {
        let ctx = EvalContext::new(None, &[]);
        let got = eval(&Expr::Literal(Value::Int(42)), &ctx).unwrap();
        assert_eq!(got, Value::Int(42));
    }

    #[test]
    fn param_scalar_returns_value() {
        let params = vec![SqlParam::Scalar(Value::Str("hi".into()))];
        let ctx = EvalContext::new(None, &params);
        let got = eval(&Expr::Param(1), &ctx).unwrap();
        assert_eq!(got, Value::Str("hi".into()));
    }

    #[test]
    fn array_collects_into_list() {
        let ctx = EvalContext::new(None, &[]);
        let got = eval(
            &Expr::Array(vec![
                Expr::Literal(Value::Int(1)),
                Expr::Literal(Value::Int(2)),
            ]),
            &ctx,
        )
        .unwrap();
        assert_eq!(got, Value::List(vec![Value::Int(1), Value::Int(2)]));
    }

    #[test]
    fn value_to_vector_accepts_floats_and_ints() {
        let v = Value::List(vec![Value::Float(0.5), Value::Int(1), Value::Float(-1.5)]);
        let got = value_to_vector(&v).unwrap();
        assert_eq!(got, vec![0.5, 1.0, -1.5]);
    }
}
