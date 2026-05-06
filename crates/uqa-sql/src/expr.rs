//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar expression evaluator: turns an [`Expr`] into a [`Value`] under
//! a row context (column -> value) and a parameter binding.

use uqa_core::Value;

use crate::ast::{BinaryOp, Expr};
use crate::error::{Result, SQLError};
use crate::params::SQLParam;
use crate::result::ResultRow;

/// Engine-side hook that the expression evaluator calls into for
/// stateful scalar functions. Currently covers sequence access
/// (`nextval` / `currval` / `setval`); the trait keeps `uqa-sql`
/// independent of `uqa-engine`.
pub trait EngineHook {
    fn nextval(&self, name: &str) -> std::result::Result<i64, String>;
    fn currval(&self, name: &str) -> std::result::Result<i64, String>;
    fn setval(&self, name: &str, value: i64) -> std::result::Result<i64, String>;
}

pub struct EvalContext<'a> {
    pub row: Option<&'a ResultRow>,
    pub params: &'a [SQLParam],
    pub engine: Option<&'a dyn EngineHook>,
}

impl<'a> EvalContext<'a> {
    pub fn new(row: Option<&'a ResultRow>, params: &'a [SQLParam]) -> Self {
        Self {
            row,
            params,
            engine: None,
        }
    }

    pub fn with_engine(mut self, engine: &'a dyn EngineHook) -> Self {
        self.engine = Some(engine);
        self
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
            Some(SQLParam::Scalar(v)) => Ok(v.clone()),
            Some(SQLParam::Vector(v)) => Ok(Value::List(
                v.iter().map(|x| Value::Float(f64::from(*x))).collect(),
            )),
            None => Err(SQLError::MissingParam(*i)),
        },
        Expr::Column(name) => {
            let row = ctx
                .row
                .ok_or_else(|| SQLError::Internal("column reference without row context".into()))?;
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
                .ok_or_else(|| SQLError::Internal("column reference without row context".into()))?;
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
        Expr::Star => Err(SQLError::Internal("`*` cannot be evaluated".into())),
        Expr::Func { name, args } => {
            // Functions registered in the operator registry (text_match,
            // knn_match, ...) are dispatched by the engine; only pure
            // scalar built-ins are evaluated inline here.
            let lower = name.to_ascii_lowercase();
            if crate::registry::is_registered(&lower) {
                return Err(SQLError::Unsupported(format!(
                    "scalar evaluation of `{name}` is not supported (use the function registry)"
                )));
            }
            let evaluated: Vec<Value> = args
                .iter()
                .map(|a| eval(a, ctx))
                .collect::<Result<Vec<_>>>()?;
            // Sequence functions need the engine hook because they
            // mutate per-engine state (`_engine._sequences` in the
            // Python reference). Routed before the pure-scalar
            // dispatch so they take precedence over any future
            // built-in named the same.
            if matches!(lower.as_str(), "nextval" | "currval" | "setval") {
                return eval_sequence_function(&lower, &evaluated, ctx);
            }
            eval_scalar_function(&lower, &evaluated)
        }
        Expr::WindowCall { name, .. } => Err(SQLError::Unsupported(format!(
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
        (lhs, rhs) => Err(SQLError::TypeMismatch(format!(
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
                return Err(SQLError::TypeMismatch("division by zero".into()));
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
fn eval_sequence_function(name: &str, args: &[Value], ctx: &EvalContext<'_>) -> Result<Value> {
    let engine = ctx.engine.ok_or_else(|| {
        SQLError::Unsupported(format!(
            "sequence function `{name}` requires an engine hook on the EvalContext"
        ))
    })?;
    if args.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "{name}() requires the sequence name"
        )));
    }
    let seq_name = value_to_string(&args[0]);
    let result: std::result::Result<i64, String> = match name {
        "nextval" => engine.nextval(&seq_name),
        "currval" => engine.currval(&seq_name),
        "setval" => {
            if args.len() < 2 {
                return Err(SQLError::TypeMismatch(
                    "setval() requires 2 arguments".into(),
                ));
            }
            let n = to_i64(&args[1])?;
            engine.setval(&seq_name, n)
        }
        other => {
            return Err(SQLError::Unsupported(format!(
                "unknown sequence function `{other}`"
            )));
        }
    };
    let v = result.map_err(SQLError::Unsupported)?;
    Ok(Value::Int(v))
}

fn eval_scalar_function(name: &str, args: &[Value]) -> Result<Value> {
    // libpg_query qualifies built-in functions as `pg_catalog.<name>`;
    // strip the schema for the dispatcher's lookup table.
    let name = name.strip_prefix("pg_catalog.").unwrap_or(name);
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
            let s = value_to_string(&args[0]);
            let from = value_to_string(&args[1]);
            let to = value_to_string(&args[2]);
            Ok(Value::Str(s.replace(&from, &to)))
        }
        "substring" | "substr" => {
            // SUBSTRING(string, start [, length]). 1-indexed per SQL.
            if args.len() < 2 || args.len() > 3 {
                return Err(SQLError::TypeMismatch("substring takes 2-3 args".into()));
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
                return Err(SQLError::TypeMismatch("left takes 2 args".into()));
            }
            let s = value_to_string(&args[0]);
            let n = to_i64(&args[1])?;
            let chars: Vec<char> = s.chars().collect();
            let take = n.clamp(0, chars.len() as i64) as usize;
            Ok(Value::Str(chars[..take].iter().collect()))
        }
        "right" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("right takes 2 args".into()));
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
            other => Err(SQLError::TypeMismatch(format!(
                "abs() expected number, got {other:?}"
            ))),
        },
        "round" => match args.len() {
            1 => match &args[0] {
                Value::Int(i) => Ok(Value::Int(*i)),
                Value::Float(f) => Ok(Value::Float(f.round())),
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!("round({other:?})"))),
            },
            2 => {
                let v = to_f64(&args[0])?;
                let places = to_i64(&args[1])?;
                let scale = 10f64.powi(places as i32);
                Ok(Value::Float((v * scale).round() / scale))
            }
            _ => Err(SQLError::TypeMismatch("round takes 1-2 args".into())),
        },
        "ceil" | "ceiling" => match &args[0] {
            Value::Int(i) => Ok(Value::Int(*i)),
            Value::Float(f) => Ok(Value::Float(f.ceil())),
            Value::Null => Ok(Value::Null),
            other => Err(SQLError::TypeMismatch(format!("ceil({other:?})"))),
        },
        "floor" => match &args[0] {
            Value::Int(i) => Ok(Value::Int(*i)),
            Value::Float(f) => Ok(Value::Float(f.floor())),
            Value::Null => Ok(Value::Null),
            other => Err(SQLError::TypeMismatch(format!("floor({other:?})"))),
        },
        "power" | "pow" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("power takes 2 args".into()));
            }
            Ok(Value::Float(to_f64(&args[0])?.powf(to_f64(&args[1])?)))
        }
        "sqrt" => Ok(Value::Float(to_f64(&args[0])?.sqrt())),
        "mod" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("mod takes 2 args".into()));
            }
            match (&args[0], &args[1]) {
                (Value::Int(a), Value::Int(b)) if *b != 0 => Ok(Value::Int(a % b)),
                (a, b) => {
                    let af = to_f64(a)?;
                    let bf = to_f64(b)?;
                    if bf == 0.0 {
                        Err(SQLError::TypeMismatch("modulo by zero".into()))
                    } else {
                        Ok(Value::Float(af % bf))
                    }
                }
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
            Ok(Value::Bool(like_match(
                &value_to_string(&args[0]),
                &value_to_string(&args[1]),
                true,
            )))
        }
        "chr" => {
            let n = to_i64(&args[0])?;
            let c = char::from_u32(n as u32)
                .ok_or_else(|| SQLError::TypeMismatch(format!("chr: invalid code point {n}")))?;
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
                    let groups: Vec<Value> = caps
                        .iter()
                        .skip(1)
                        .map(|m| {
                            m.map(|x| Value::Str(x.as_str().into()))
                                .unwrap_or(Value::Null)
                        })
                        .collect();
                    if groups.is_empty() {
                        Ok(Value::Str(caps.get(0).unwrap().as_str().into()))
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
        // Trig / math
        "sin" => Ok(Value::Float(to_f64(&args[0])?.sin())),
        "cos" => Ok(Value::Float(to_f64(&args[0])?.cos())),
        "tan" => Ok(Value::Float(to_f64(&args[0])?.tan())),
        "asin" => Ok(Value::Float(to_f64(&args[0])?.asin())),
        "acos" => Ok(Value::Float(to_f64(&args[0])?.acos())),
        "atan" => Ok(Value::Float(to_f64(&args[0])?.atan())),
        "atan2" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("atan2 takes 2 args".into()));
            }
            Ok(Value::Float(to_f64(&args[0])?.atan2(to_f64(&args[1])?)))
        }
        "sinh" => Ok(Value::Float(to_f64(&args[0])?.sinh())),
        "cosh" => Ok(Value::Float(to_f64(&args[0])?.cosh())),
        "tanh" => Ok(Value::Float(to_f64(&args[0])?.tanh())),
        "exp" => Ok(Value::Float(to_f64(&args[0])?.exp())),
        "ln" => Ok(Value::Float(to_f64(&args[0])?.ln())),
        "log" | "log10" => match args.len() {
            1 => Ok(Value::Float(to_f64(&args[0])?.log10())),
            2 => {
                let base = to_f64(&args[0])?;
                let v = to_f64(&args[1])?;
                Ok(Value::Float(v.log(base)))
            }
            _ => Err(SQLError::TypeMismatch("log takes 1 or 2 args".into())),
        },
        "log2" => Ok(Value::Float(to_f64(&args[0])?.log2())),
        "cbrt" => Ok(Value::Float(to_f64(&args[0])?.cbrt())),
        "sign" => Ok(Value::Int(match to_f64(&args[0])? {
            v if v > 0.0 => 1,
            v if v < 0.0 => -1,
            _ => 0,
        })),
        "trunc" => match args.len() {
            1 => Ok(Value::Float(to_f64(&args[0])?.trunc())),
            2 => {
                let v = to_f64(&args[0])?;
                let p = to_i64(&args[1])?;
                let scale = 10f64.powi(p as i32);
                Ok(Value::Float((v * scale).trunc() / scale))
            }
            _ => Err(SQLError::TypeMismatch("trunc takes 1 or 2 args".into())),
        },
        "pi" => Ok(Value::Float(std::f64::consts::PI)),
        "degrees" => Ok(Value::Float(to_f64(&args[0])?.to_degrees())),
        "radians" => Ok(Value::Float(to_f64(&args[0])?.to_radians())),
        "random" => {
            // Deterministic-ish pseudo random based on system time so
            // tests can stub it; the Python reference also wraps the
            // platform RNG.
            use std::time::{SystemTime, UNIX_EPOCH};
            let t = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0) as f64;
            Ok(Value::Float((t.sin().abs() * 1.0e9).fract()))
        }
        // Padding / formatting
        "lpad" | "rpad" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(SQLError::TypeMismatch("[lr]pad takes 2-3 args".into()));
            }
            let s = value_to_string(&args[0]);
            let n = to_i64(&args[1])?.max(0) as usize;
            let fill = args
                .get(2)
                .map(value_to_string)
                .unwrap_or_else(|| " ".into());
            let chars: Vec<char> = s.chars().collect();
            if chars.len() >= n {
                return Ok(Value::Str(chars[..n].iter().collect()));
            }
            let need = n - chars.len();
            let fill_chars: Vec<char> = fill.chars().collect();
            if fill_chars.is_empty() {
                return Ok(Value::Str(s));
            }
            let mut padding: String = String::with_capacity(need);
            for i in 0..need {
                padding.push(fill_chars[i % fill_chars.len()]);
            }
            Ok(Value::Str(if name == "lpad" {
                format!("{padding}{s}")
            } else {
                format!("{s}{padding}")
            }))
        }
        "repeat" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("repeat takes 2 args".into()));
            }
            let s = value_to_string(&args[0]);
            let n = to_i64(&args[1])?.max(0) as usize;
            Ok(Value::Str(s.repeat(n)))
        }
        "translate" => {
            if args.len() != 3 {
                return Err(SQLError::TypeMismatch("translate takes 3 args".into()));
            }
            let s = value_to_string(&args[0]);
            let from: Vec<char> = value_to_string(&args[1]).chars().collect();
            let to: Vec<char> = value_to_string(&args[2]).chars().collect();
            let mapped: String = s
                .chars()
                .filter_map(|c| match from.iter().position(|x| *x == c) {
                    Some(i) if i < to.len() => Some(to[i]),
                    Some(_) => None,
                    None => Some(c),
                })
                .collect();
            Ok(Value::Str(mapped))
        }
        "overlay" => {
            // OVERLAY(string PLACING substring FROM start [FOR length])
            if args.len() < 3 || args.len() > 4 {
                return Err(SQLError::TypeMismatch("overlay takes 3 or 4 args".into()));
            }
            let s: Vec<char> = value_to_string(&args[0]).chars().collect();
            let placing: Vec<char> = value_to_string(&args[1]).chars().collect();
            let start = to_i64(&args[2])?.max(1) as usize - 1;
            let len = if args.len() == 4 {
                to_i64(&args[3])?.max(0) as usize
            } else {
                placing.len()
            };
            let end = (start + len).min(s.len());
            let mut out: String = s[..start.min(s.len())].iter().collect();
            out.push_str(&placing.iter().collect::<String>());
            out.push_str(&s[end..].iter().collect::<String>());
            Ok(Value::Str(out))
        }
        "format" => {
            // FORMAT('hello %s', name) -- minimal printf-style %s/%d
            // substitution. Mirrors enough of Postgres FORMAT for the
            // common cases.
            if args.is_empty() {
                return Err(SQLError::TypeMismatch(
                    "format needs a format string".into(),
                ));
            }
            let fmt = value_to_string(&args[0]);
            let mut out = String::with_capacity(fmt.len());
            let mut iter = fmt.chars().peekable();
            let mut idx = 1usize;
            while let Some(c) = iter.next() {
                if c == '%' {
                    match iter.next() {
                        Some('s') | Some('I') | Some('L') => {
                            out.push_str(&value_to_string(args.get(idx).unwrap_or(&Value::Null)));
                            idx += 1;
                        }
                        Some('d') => {
                            let n = args.get(idx).and_then(|v| coerce_i64(v)).unwrap_or(0);
                            out.push_str(&n.to_string());
                            idx += 1;
                        }
                        Some('%') => out.push('%'),
                        Some(other) => out.push(other),
                        None => out.push('%'),
                    }
                } else {
                    out.push(c);
                }
            }
            Ok(Value::Str(out))
        }
        "md5" => Err(SQLError::Unsupported(
            "md5() is not yet wired -- pull in the `md-5` crate or call \
             a stdlib hashing helper at the engine boundary"
                .into(),
        )),
        "encode" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("encode takes 2 args".into()));
            }
            let bytes = value_to_string(&args[0]);
            let encoding = value_to_string(&args[1]);
            match encoding.as_str() {
                "hex" => Ok(Value::Str(
                    bytes.bytes().map(|b| format!("{b:02x}")).collect(),
                )),
                "escape" => Ok(Value::Str(bytes.escape_default().collect())),
                "base64" => Ok(Value::Str(base64_encode(bytes.as_bytes()))),
                other => Err(SQLError::TypeMismatch(format!(
                    "unknown encoding {other:?}"
                ))),
            }
        }
        "decode" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("decode takes 2 args".into()));
            }
            let s = value_to_string(&args[0]);
            let encoding = value_to_string(&args[1]);
            match encoding.as_str() {
                "hex" => {
                    let mut out = Vec::with_capacity(s.len() / 2);
                    let bytes = s.as_bytes();
                    let mut i = 0;
                    while i + 1 < bytes.len() {
                        let hi = (bytes[i] as char).to_digit(16).unwrap_or(0) as u8;
                        let lo = (bytes[i + 1] as char).to_digit(16).unwrap_or(0) as u8;
                        out.push(hi * 16 + lo);
                        i += 2;
                    }
                    Ok(Value::Str(String::from_utf8_lossy(&out).to_string()))
                }
                "base64" => base64_decode(&s)
                    .map(|b| Value::Str(String::from_utf8_lossy(&b).to_string()))
                    .map_err(|e| SQLError::TypeMismatch(format!("base64 decode: {e}"))),
                other => Err(SQLError::TypeMismatch(format!(
                    "unknown encoding {other:?}"
                ))),
            }
        }
        "split_part" => {
            if args.len() != 3 {
                return Err(SQLError::TypeMismatch("split_part takes 3 args".into()));
            }
            let s = value_to_string(&args[0]);
            let sep = value_to_string(&args[1]);
            let idx = to_i64(&args[2])?;
            let parts: Vec<&str> = if sep.is_empty() {
                vec![s.as_str()]
            } else {
                s.split(sep.as_str()).collect()
            };
            let idx_usize = if idx >= 1 {
                (idx - 1) as usize
            } else {
                return Ok(Value::Str(String::new()));
            };
            Ok(Value::Str(
                parts.get(idx_usize).copied().unwrap_or("").to_string(),
            ))
        }
        "now" | "current_timestamp" => {
            use chrono::Utc;
            Ok(Value::Str(
                Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            ))
        }
        "current_date" => {
            use chrono::Utc;
            Ok(Value::Str(Utc::now().format("%Y-%m-%d").to_string()))
        }
        "to_timestamp" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("to_timestamp takes 1 arg".into()));
            }
            let secs = to_f64(&args[0])?;
            let ns = ((secs.fract() * 1e9).round() as i64).rem_euclid(1_000_000_000);
            let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, ns as u32)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!("to_timestamp out of range {secs}"))
                })?;
            Ok(Value::Str(
                dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            ))
        }
        "extract" | "date_part" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch(
                    "extract takes 2 args (field, ts)".into(),
                ));
            }
            let field = value_to_string(&args[0]).to_ascii_lowercase();
            let ts_str = value_to_string(&args[1]);
            let dt = parse_timestamp(&ts_str)?;
            extract_field(&field, &dt)
        }
        "age" => {
            let now = chrono::Utc::now();
            let (a, b) = match args.len() {
                1 => (parse_timestamp(&value_to_string(&args[0]))?, now),
                2 => (
                    parse_timestamp(&value_to_string(&args[0]))?,
                    parse_timestamp(&value_to_string(&args[1]))?,
                ),
                _ => return Err(SQLError::TypeMismatch("age takes 1-2 args".into())),
            };
            Ok(Value::Float((a - b).num_milliseconds() as f64 / 1000.0))
        }
        "date_trunc" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("date_trunc takes 2 args".into()));
            }
            let unit = value_to_string(&args[0]).to_ascii_lowercase();
            let dt = parse_timestamp(&value_to_string(&args[1]))?;
            date_trunc(&unit, &dt)
        }
        "make_timestamp" => {
            if !(6..=7).contains(&args.len()) {
                return Err(SQLError::TypeMismatch(
                    "make_timestamp takes 6-7 args".into(),
                ));
            }
            let year = to_i64(&args[0])? as i32;
            let month = to_i64(&args[1])? as u32;
            let day = to_i64(&args[2])? as u32;
            let hour = to_i64(&args[3])? as u32;
            let minute = to_i64(&args[4])? as u32;
            let second = to_f64(&args[5])?;
            make_timestamp(year, month, day, hour, minute, second)
        }
        "make_date" => {
            if args.len() != 3 {
                return Err(SQLError::TypeMismatch("make_date takes 3 args".into()));
            }
            let year = to_i64(&args[0])? as i32;
            let month = to_i64(&args[1])? as u32;
            let day = to_i64(&args[2])? as u32;
            chrono::NaiveDate::from_ymd_opt(year, month, day)
                .map(|d| Value::Str(d.format("%Y-%m-%d").to_string()))
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!(
                        "make_date: invalid date {year:04}-{month:02}-{day:02}"
                    ))
                })
        }
        "make_interval" => {
            // make_interval(years, months, weeks, days, hours, mins, secs)
            // Returns ISO 8601 duration P[n]Y[n]M[n]DT[n]H[n]M[n]S.
            let mut iso = String::from("P");
            let years = args.first().map(to_i64).transpose()?.unwrap_or(0);
            let months = args.get(1).map(to_i64).transpose()?.unwrap_or(0);
            let weeks = args.get(2).map(to_i64).transpose()?.unwrap_or(0);
            let days = args.get(3).map(to_i64).transpose()?.unwrap_or(0);
            let hours = args.get(4).map(to_i64).transpose()?.unwrap_or(0);
            let mins = args.get(5).map(to_i64).transpose()?.unwrap_or(0);
            let secs = args.get(6).map(to_f64).transpose()?.unwrap_or(0.0);
            use std::fmt::Write;
            if years != 0 {
                let _ = write!(iso, "{years}Y");
            }
            if months != 0 {
                let _ = write!(iso, "{months}M");
            }
            let total_days = weeks * 7 + days;
            if total_days != 0 {
                let _ = write!(iso, "{total_days}D");
            }
            if hours != 0 || mins != 0 || secs != 0.0 {
                iso.push('T');
                if hours != 0 {
                    let _ = write!(iso, "{hours}H");
                }
                if mins != 0 {
                    let _ = write!(iso, "{mins}M");
                }
                if secs != 0.0 {
                    let _ = write!(iso, "{secs}S");
                }
            }
            if iso == "P" {
                iso.push_str("0D");
            }
            Ok(Value::Str(iso))
        }
        "to_char" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("to_char takes 2 args".into()));
            }
            let fmt = value_to_string(&args[1]);
            match &args[0] {
                Value::Int(i) => Ok(Value::Str(format_pg_number(*i as f64, &fmt))),
                Value::Float(f) => Ok(Value::Str(format_pg_number(*f, &fmt))),
                Value::Str(s) => {
                    let dt = parse_timestamp(s)?;
                    Ok(Value::Str(format_pg_datetime(&dt, &fmt)))
                }
                other => Err(SQLError::TypeMismatch(format!(
                    "to_char: unsupported source {other:?}"
                ))),
            }
        }
        "to_date" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("to_date takes 2 args".into()));
            }
            let s = value_to_string(&args[0]);
            let fmt = pg_to_chrono_fmt(&value_to_string(&args[1]));
            let date = chrono::NaiveDate::parse_from_str(&s, &fmt)
                .map_err(|e| SQLError::TypeMismatch(format!("to_date: {e}")))?;
            Ok(Value::Str(date.format("%Y-%m-%d").to_string()))
        }
        "to_number" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("to_number takes 2 args".into()));
            }
            let s = value_to_string(&args[0]);
            let cleaned: String = s
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
                .collect();
            cleaned
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|e| SQLError::TypeMismatch(format!("to_number: {e}")))
        }
        "isfinite" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("isfinite takes 1 arg".into()));
            }
            match &args[0] {
                Value::Float(f) => Ok(Value::Bool(f.is_finite())),
                Value::Int(_) | Value::Str(_) => Ok(Value::Bool(true)),
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!(
                    "isfinite: unsupported {other:?}"
                ))),
            }
        }
        "clock_timestamp" | "statement_timestamp" => Ok(Value::Str(
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        )),
        "timeofday" => Ok(Value::Str(
            chrono::Utc::now()
                .format("%a %b %d %H:%M:%S%.6f %Y UTC")
                .to_string(),
        )),
        "typeof" | "pg_typeof" => Ok(Value::Str(typeof_value(&args[0]))),
        "gen_random_uuid" => {
            // Time + counter-based UUIDv4-like (not RFC 4122 cryptographically
            // strong, but unique per call within a process). Used for
            // expression-time UUID generation only.
            Ok(Value::Str(generate_random_uuid()))
        }
        // -------------------------------------------------------------
        // JSON functions
        // -------------------------------------------------------------
        "json_build_object" | "jsonb_build_object" => json_build_object(args),
        "json_build_array" | "jsonb_build_array" => Ok(Value::Str(
            serde_json::to_string(&args.iter().map(value_to_json).collect::<Vec<_>>())
                .map_err(|e| SQLError::TypeMismatch(format!("json_build_array: {e}")))?,
        )),
        "json_typeof" | "jsonb_typeof" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("json_typeof takes 1 arg".into()));
            }
            let parsed = parse_json(&value_to_string(&args[0]))?;
            Ok(Value::Str(json_typeof(&parsed).to_string()))
        }
        "json_array_length" | "jsonb_array_length" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch(
                    "json_array_length takes 1 arg".into(),
                ));
            }
            let parsed = parse_json(&value_to_string(&args[0]))?;
            match parsed {
                serde_json::Value::Array(arr) => Ok(Value::Int(arr.len() as i64)),
                _ => Err(SQLError::TypeMismatch(
                    "json_array_length: argument is not an array".into(),
                )),
            }
        }
        "json_extract_path" | "jsonb_extract_path" => json_extract_path(args, false),
        "json_extract_path_text" | "jsonb_extract_path_text" => json_extract_path(args, true),
        "to_json" | "to_jsonb" | "row_to_json" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("to_json takes 1 arg".into()));
            }
            let json = value_to_json(&args[0]);
            Ok(Value::Str(serde_json::to_string(&json).map_err(|e| {
                SQLError::TypeMismatch(format!("to_json: {e}"))
            })?))
        }
        "jsonb_set" | "jsonb_insert" => jsonb_set(args),
        "json_strip_nulls" | "jsonb_strip_nulls" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch(
                    "json_strip_nulls takes 1 arg".into(),
                ));
            }
            let mut parsed = parse_json(&value_to_string(&args[0]))?;
            strip_nulls(&mut parsed);
            Ok(Value::Str(serde_json::to_string(&parsed).map_err(|e| {
                SQLError::TypeMismatch(format!("json_strip_nulls: {e}"))
            })?))
        }
        "json_object_keys" | "jsonb_object_keys" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch(
                    "json_object_keys takes 1 arg".into(),
                ));
            }
            let parsed = parse_json(&value_to_string(&args[0]))?;
            match parsed {
                serde_json::Value::Object(map) => Ok(Value::List(
                    map.keys().map(|k| Value::Str(k.clone())).collect(),
                )),
                _ => Err(SQLError::TypeMismatch(
                    "json_object_keys: argument is not an object".into(),
                )),
            }
        }
        // -------------------------------------------------------------
        // Array functions
        // -------------------------------------------------------------
        "array_length" | "array_upper" => {
            if args.is_empty() {
                return Err(SQLError::TypeMismatch("array_length takes >= 1 arg".into()));
            }
            match &args[0] {
                Value::List(items) => Ok(Value::Int(items.len() as i64)),
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!(
                    "array_length: not an array {other:?}"
                ))),
            }
        }
        "array_lower" => Ok(Value::Int(1)),
        "cardinality" => match &args[0] {
            Value::List(items) => Ok(Value::Int(items.len() as i64)),
            Value::Null => Ok(Value::Null),
            other => Err(SQLError::TypeMismatch(format!(
                "cardinality: not an array {other:?}"
            ))),
        },
        "array_cat" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("array_cat takes 2 args".into()));
            }
            match (&args[0], &args[1]) {
                (Value::List(a), Value::List(b)) => {
                    let mut out = a.clone();
                    out.extend(b.iter().cloned());
                    Ok(Value::List(out))
                }
                _ => Err(SQLError::TypeMismatch(
                    "array_cat: both args must be arrays".into(),
                )),
            }
        }
        "array_append" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("array_append takes 2 args".into()));
            }
            match &args[0] {
                Value::List(items) => {
                    let mut out = items.clone();
                    out.push(args[1].clone());
                    Ok(Value::List(out))
                }
                Value::Null => Ok(Value::List(vec![args[1].clone()])),
                other => Err(SQLError::TypeMismatch(format!(
                    "array_append: not an array {other:?}"
                ))),
            }
        }
        "array_prepend" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("array_prepend takes 2 args".into()));
            }
            match &args[1] {
                Value::List(items) => {
                    let mut out = vec![args[0].clone()];
                    out.extend(items.iter().cloned());
                    Ok(Value::List(out))
                }
                Value::Null => Ok(Value::List(vec![args[0].clone()])),
                other => Err(SQLError::TypeMismatch(format!(
                    "array_prepend: not an array {other:?}"
                ))),
            }
        }
        "array_remove" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("array_remove takes 2 args".into()));
            }
            match &args[0] {
                Value::List(items) => Ok(Value::List(
                    items.iter().filter(|v| **v != args[1]).cloned().collect(),
                )),
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!(
                    "array_remove: not an array {other:?}"
                ))),
            }
        }
        "array_position" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("array_position takes 2 args".into()));
            }
            match &args[0] {
                Value::List(items) => Ok(items
                    .iter()
                    .position(|v| *v == args[1])
                    .map(|i| Value::Int((i + 1) as i64))
                    .unwrap_or(Value::Null)),
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!(
                    "array_position: not an array {other:?}"
                ))),
            }
        }
        "unnest" => match &args[0] {
            Value::List(items) => Ok(Value::List(items.clone())),
            Value::Null => Ok(Value::List(Vec::new())),
            other => Err(SQLError::TypeMismatch(format!(
                "unnest: not an array {other:?}"
            ))),
        },
        // -------------------------------------------------------------
        // Geospatial primitives (point, distance, within, dwithin)
        // -------------------------------------------------------------
        "point" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("point takes 2 args".into()));
            }
            let x = to_f64(&args[0])?;
            let y = to_f64(&args[1])?;
            Ok(Value::List(vec![Value::Float(x), Value::Float(y)]))
        }
        "st_distance" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("st_distance takes 2 args".into()));
            }
            let (x1, y1) = point_xy(&args[0])?;
            let (x2, y2) = point_xy(&args[1])?;
            Ok(Value::Float(((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()))
        }
        "st_within" | "st_dwithin" => {
            // Polygon containment is not yet modeled; for now interpret
            // dwithin as a Euclidean radius check matching Python's
            // simplified semantics.
            if args.len() < 2 {
                return Err(SQLError::TypeMismatch(format!("{name} takes 2-3 args")));
            }
            let (x1, y1) = point_xy(&args[0])?;
            let (x2, y2) = point_xy(&args[1])?;
            let d = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
            let radius = if args.len() == 3 {
                to_f64(&args[2])?
            } else {
                0.0
            };
            Ok(Value::Bool(d <= radius))
        }
        "overlaps" => {
            if args.len() != 4 {
                return Err(SQLError::TypeMismatch(
                    "overlaps takes 4 args (start1, end1, start2, end2)".into(),
                ));
            }
            let s1 = parse_timestamp(&value_to_string(&args[0]))?;
            let e1 = parse_timestamp(&value_to_string(&args[1]))?;
            let s2 = parse_timestamp(&value_to_string(&args[2]))?;
            let e2 = parse_timestamp(&value_to_string(&args[3]))?;
            Ok(Value::Bool(s1 < e2 && s2 < e1))
        }
        other => Err(SQLError::Unsupported(format!("scalar function `{other}`"))),
    }
}

// --------------------------------------------------------------------
// JSON helpers
// --------------------------------------------------------------------

fn parse_json(s: &str) -> Result<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(s)
        .map_err(|e| SQLError::TypeMismatch(format!("invalid JSON: {e}")))
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Str(s) => {
            // Try to parse strings already containing JSON; otherwise
            // wrap as a JSON string. Mirrors Python `to_json` semantics
            // for nested expressions.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                if matches!(
                    parsed,
                    serde_json::Value::Object(_) | serde_json::Value::Array(_)
                ) {
                    return parsed;
                }
            }
            serde_json::Value::String(s.clone())
        }
        Value::Bytes(b) => serde_json::Value::String(format!("0x{}", hex_encode(b))),
        Value::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Map(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                obj.insert(k.clone(), value_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
    }
}

#[allow(dead_code)]
fn json_to_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(arr) => Value::List(arr.iter().map(json_to_value).collect()),
        serde_json::Value::Object(obj) => {
            let mut map = std::collections::BTreeMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), json_to_value(v));
            }
            Value::Map(map)
        }
    }
}

fn json_typeof(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn json_build_object(args: &[Value]) -> Result<Value> {
    if args.len() % 2 != 0 {
        return Err(SQLError::TypeMismatch(
            "json_build_object requires an even number of args".into(),
        ));
    }
    let mut obj = serde_json::Map::new();
    for chunk in args.chunks_exact(2) {
        let key = value_to_string(&chunk[0]);
        let val = value_to_json(&chunk[1]);
        obj.insert(key, val);
    }
    serde_json::to_string(&serde_json::Value::Object(obj))
        .map(Value::Str)
        .map_err(|e| SQLError::TypeMismatch(format!("json_build_object: {e}")))
}

fn json_extract_path(args: &[Value], as_text: bool) -> Result<Value> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            "json_extract_path takes 2+ args".into(),
        ));
    }
    let mut current = parse_json(&value_to_string(&args[0]))?;
    for key in &args[1..] {
        let key_str = value_to_string(key);
        current = match current {
            serde_json::Value::Object(mut obj) => {
                obj.remove(&key_str).unwrap_or(serde_json::Value::Null)
            }
            serde_json::Value::Array(arr) => match key_str.parse::<usize>() {
                Ok(idx) => arr.into_iter().nth(idx).unwrap_or(serde_json::Value::Null),
                Err(_) => serde_json::Value::Null,
            },
            _ => serde_json::Value::Null,
        };
    }
    if as_text {
        Ok(Value::Str(match current {
            serde_json::Value::String(s) => s,
            serde_json::Value::Null => return Ok(Value::Null),
            other => serde_json::to_string(&other).unwrap_or_default(),
        }))
    } else if matches!(current, serde_json::Value::Null) {
        Ok(Value::Null)
    } else {
        Ok(Value::Str(
            serde_json::to_string(&current).unwrap_or_default(),
        ))
    }
}

fn jsonb_set(args: &[Value]) -> Result<Value> {
    if !(3..=4).contains(&args.len()) {
        return Err(SQLError::TypeMismatch("jsonb_set takes 3-4 args".into()));
    }
    let mut current = parse_json(&value_to_string(&args[0]))?;
    let path: Vec<String> = match &args[1] {
        Value::List(items) => items.iter().map(value_to_string).collect(),
        Value::Str(s) => {
            // Accept "{a,b,c}" PostgreSQL array literal.
            s.trim_matches(|c| c == '{' || c == '}')
                .split(',')
                .map(|s| s.trim().to_string())
                .collect()
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "jsonb_set: path must be array, got {other:?}"
            )));
        }
    };
    let new_val = parse_json(&value_to_string(&args[2]))
        .unwrap_or_else(|_| serde_json::Value::String(value_to_string(&args[2])));
    json_set_path(&mut current, &path, new_val);
    Ok(Value::Str(serde_json::to_string(&current).map_err(
        |e| SQLError::TypeMismatch(format!("jsonb_set: {e}")),
    )?))
}

fn json_set_path(current: &mut serde_json::Value, path: &[String], new_val: serde_json::Value) {
    if path.is_empty() {
        *current = new_val;
        return;
    }
    let head = &path[0];
    let rest = &path[1..];
    match current {
        serde_json::Value::Object(obj) => {
            let entry = obj.entry(head.clone()).or_insert(serde_json::Value::Null);
            json_set_path(entry, rest, new_val);
        }
        serde_json::Value::Array(arr) => {
            if let Ok(idx) = head.parse::<usize>() {
                while arr.len() <= idx {
                    arr.push(serde_json::Value::Null);
                }
                json_set_path(&mut arr[idx], rest, new_val);
            }
        }
        _ => {
            let mut new_obj = serde_json::Map::new();
            new_obj.insert(head.clone(), serde_json::Value::Null);
            let mut wrapper = serde_json::Value::Object(new_obj);
            json_set_path(&mut wrapper, path, new_val);
            *current = wrapper;
        }
    }
}

fn strip_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(obj) => {
            obj.retain(|_, v| !v.is_null());
            for v in obj.values_mut() {
                strip_nulls(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                strip_nulls(v);
            }
        }
        _ => {}
    }
}

fn typeof_value(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Int(_) => "integer".into(),
        Value::Float(_) => "double precision".into(),
        Value::Str(_) => "text".into(),
        Value::Bytes(_) => "bytea".into(),
        Value::List(_) => "array".into(),
        Value::Map(_) => "jsonb".into(),
    }
}

fn point_xy(v: &Value) -> Result<(f64, f64)> {
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

fn date_trunc(unit: &str, dt: &chrono::DateTime<chrono::Utc>) -> Result<Value> {
    use chrono::{Datelike, NaiveDate, TimeZone, Timelike, Utc};
    let truncated = match unit {
        "year" => Utc
            .with_ymd_and_hms(dt.year(), 1, 1, 0, 0, 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad year".into()))?,
        "quarter" => {
            let q_start_month = ((dt.month() - 1) / 3) * 3 + 1;
            Utc.with_ymd_and_hms(dt.year(), q_start_month, 1, 0, 0, 0)
                .single()
                .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad quarter".into()))?
        }
        "month" => Utc
            .with_ymd_and_hms(dt.year(), dt.month(), 1, 0, 0, 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad month".into()))?,
        "week" => {
            // Truncate to the Monday of the ISO week.
            let weekday_offset = dt.weekday().num_days_from_monday() as i64;
            let date = NaiveDate::from_ymd_opt(dt.year(), dt.month(), dt.day())
                .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad week".into()))?
                - chrono::Duration::days(weekday_offset);
            let naive = date.and_hms_opt(0, 0, 0).unwrap();
            Utc.from_utc_datetime(&naive)
        }
        "day" => Utc
            .with_ymd_and_hms(dt.year(), dt.month(), dt.day(), 0, 0, 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad day".into()))?,
        "hour" => Utc
            .with_ymd_and_hms(dt.year(), dt.month(), dt.day(), dt.hour(), 0, 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad hour".into()))?,
        "minute" => Utc
            .with_ymd_and_hms(dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(), 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad minute".into()))?,
        "second" => Utc
            .with_ymd_and_hms(
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second(),
            )
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad second".into()))?,
        other => {
            return Err(SQLError::Unsupported(format!("date_trunc unit `{other}`")));
        }
    };
    Ok(Value::Str(
        truncated.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
    ))
}

fn make_timestamp(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: f64,
) -> Result<Value> {
    use chrono::{NaiveDate, TimeZone, Utc};
    let secs = second.trunc() as u32;
    let nanos = (second.fract() * 1e9).round() as u32;
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| SQLError::TypeMismatch("make_timestamp: bad date".into()))?;
    let naive = date
        .and_hms_nano_opt(hour, minute, secs, nanos)
        .ok_or_else(|| SQLError::TypeMismatch("make_timestamp: bad time".into()))?;
    let dt = Utc.from_utc_datetime(&naive);
    Ok(Value::Str(
        dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
    ))
}

fn pg_to_chrono_fmt(fmt: &str) -> String {
    // Translate a small subset of PostgreSQL `to_date` template tokens
    // into chrono format specifiers. The Python reference relies on
    // datetime.strptime; this routine mirrors the most common patterns
    // (`YYYY`, `MM`, `DD`, `HH24`, `MI`, `SS`).
    fmt.replace("YYYY", "%Y")
        .replace("YY", "%y")
        .replace("MM", "%m")
        .replace("DD", "%d")
        .replace("HH24", "%H")
        .replace("HH12", "%I")
        .replace("MI", "%M")
        .replace("SS", "%S")
}

fn format_pg_number(n: f64, fmt: &str) -> String {
    // Minimal `to_char(numeric, '999...')` support: count `9` digits
    // and zero-pad the integral part. Falls back to plain Display.
    let digits = fmt.chars().filter(|c| *c == '9' || *c == '0').count();
    if digits == 0 {
        return n.to_string();
    }
    let int_part = n.trunc() as i64;
    if fmt.contains('.') {
        let frac_digits = fmt.split('.').nth(1).map(str::len).unwrap_or(0);
        format!("{n:.frac_digits$}")
    } else {
        format!("{int_part:0digits$}")
    }
}

fn format_pg_datetime(dt: &chrono::DateTime<chrono::Utc>, fmt: &str) -> String {
    dt.format(&pg_to_chrono_fmt(fmt)).to_string()
}

fn generate_random_uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&now_ns.to_be_bytes());
    bytes[8..].copy_from_slice(&counter.to_be_bytes());
    // Set version 4 + variant per RFC 4122.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parse a timestamp string into a UTC `DateTime`. Accepts:
/// - RFC3339 (`2025-01-31T12:00:00Z`, `2025-01-31T12:00:00+09:00`)
/// - PostgreSQL-ish `YYYY-MM-DD HH:MM:SS[.fff]` (assumed UTC)
/// - Bare date `YYYY-MM-DD` (midnight UTC).
fn parse_timestamp(s: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    let formats: &[&str] = &[
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ];
    for fmt in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(Utc.from_utc_datetime(&naive));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let naive = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| SQLError::TypeMismatch(format!("bad date {s}")))?;
        return Ok(Utc.from_utc_datetime(&naive));
    }
    Err(SQLError::TypeMismatch(format!(
        "cannot parse timestamp {s:?}"
    )))
}

/// `EXTRACT(field FROM ts)` field selectors. Mirrors the Python
/// reference's `_sf_extract` implementation.
fn extract_field(field: &str, dt: &chrono::DateTime<chrono::Utc>) -> Result<Value> {
    use chrono::{Datelike, Timelike};
    match field {
        "year" => Ok(Value::Int(i64::from(dt.year()))),
        "month" => Ok(Value::Int(i64::from(dt.month()))),
        "day" => Ok(Value::Int(i64::from(dt.day()))),
        "hour" => Ok(Value::Int(i64::from(dt.hour()))),
        "minute" => Ok(Value::Int(i64::from(dt.minute()))),
        "second" => {
            let secs = f64::from(dt.second()) + f64::from(dt.nanosecond()) / 1_000_000_000.0;
            Ok(Value::Float(secs))
        }
        "millisecond" => Ok(Value::Int(i64::from(dt.timestamp_subsec_millis()))),
        "microsecond" => Ok(Value::Int(i64::from(dt.timestamp_subsec_micros()))),
        "dow" => Ok(Value::Int(i64::from(dt.weekday().num_days_from_sunday()))),
        "isodow" => Ok(Value::Int(i64::from(dt.weekday().number_from_monday()))),
        "doy" => Ok(Value::Int(i64::from(dt.ordinal()))),
        "epoch" => Ok(Value::Float(
            dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_micros()) / 1_000_000.0,
        )),
        "quarter" => Ok(Value::Int(i64::from(dt.month() - 1) / 3 + 1)),
        "week" => Ok(Value::Int(i64::from(dt.iso_week().week()))),
        other => Err(SQLError::Unsupported(format!("EXTRACT field `{other}`"))),
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
        other => Err(SQLError::Unsupported(format!("CAST AS {other}"))),
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
        .ok_or_else(|| SQLError::TypeMismatch(format!("missing arg #{idx}")))
}

fn string1<F: FnOnce(&str) -> String>(args: &[Value], f: F) -> Result<Value> {
    if args.is_empty() {
        return Err(SQLError::TypeMismatch("string fn needs 1 arg".into()));
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
            .map_err(|_| SQLError::TypeMismatch(format!("cannot parse {s:?} as integer"))),
        other => Err(SQLError::TypeMismatch(format!(
            "expected integer, got {other:?}"
        ))),
    }
}

fn to_f64(v: &Value) -> Result<f64> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

/// Best-effort `Value -> i64`. Returns `None` for shapes that do not
/// have a well-defined integer projection (e.g. `Value::Null`).
fn coerce_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        Value::Float(f) => Some(*f as i64),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Str(s) => s.parse().ok(),
        _ => None,
    }
}

// -------------------------------------------------------------------------
// MD5 stub. The reference port carried a hand-written implementation
// here; it shipped with a transcription error in the constant table
// that broke its self-test. The builtin is surfaced as Unsupported
// for now; production callers should feed `md5()` data through the
// `md-5` crate at the engine boundary.
// -------------------------------------------------------------------------

#[allow(dead_code)]
fn md5_hex(input: &[u8]) -> String {
    let digest = md5_compute(input);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[allow(dead_code, clippy::many_single_char_names)]
fn md5_compute(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76a_a478,
        0xe8c7_b756,
        0x2420_70db,
        0xc1bd_ceee,
        0xf57c_0faf,
        0x4787_c62a,
        0xa830_4613,
        0xfd46_9501,
        0x6980_98d8,
        0x8b44_f7af,
        0xffff_5bb1,
        0x895c_d7be,
        0x6b90_1122,
        0xfd98_7193,
        0xa679_438e,
        0x49b4_0821,
        0xf61e_2562,
        0xc040_b340,
        0x265e_5a51,
        0xe9b6_c7aa,
        0xd62f_105d,
        0x0244_1453,
        0xd8a1_e681,
        0xe7d3_fbc8,
        0x21e1_cde6,
        0xc337_07d6,
        0xf4d5_0d87,
        0x455a_14ed,
        0xa9e3_e905,
        0xfcef_a3f8,
        0x676f_02d9,
        0x8d2a_4c8a,
        0xfffa_3942,
        0x8771_f681,
        0x6d9d_6122,
        0xfde5_380c,
        0xa4be_ea44,
        0x4bde_cfa9,
        0xf6bb_4b60,
        0xbebf_bc70,
        0x289b_7ec6,
        0xeaa1_27fa,
        0xd4ef_3085,
        0x0488_1d05,
        0xd9d4_d039,
        0xe6db_99e5,
        0x1fa2_7cf8,
        0xc4ac_5665,
        0xf429_2244,
        0x432a_ff97,
        0xab94_23a7,
        0xfc93_a039,
        0x655b_59c3,
        0x8f0c_cc92,
        0xffef_f47d,
        0x8584_5dd1,
        0x6fa8_7e4f,
        0xfe2c_e6e0,
        0xa301_4314,
        0x4e08_11a1,
        0xf753_7e82,
        0xbd3a_f235,
        0x2ad7_d2bb,
        0xeb86_d391,
    ];

    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    let mut buf: Vec<u8> = Vec::with_capacity(input.len() + 72);
    buf.extend_from_slice(input);
    buf.push(0x80);
    while buf.len() % 64 != 56 {
        buf.push(0);
    }
    let bits = (input.len() as u64).wrapping_mul(8);
    buf.extend_from_slice(&bits.to_le_bytes());

    for chunk in buf.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

// -------------------------------------------------------------------------
// Minimal base64 (RFC 4648). Used only for the SQL `encode` /
// `decode` builtins; not performance-critical.
// -------------------------------------------------------------------------

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(BASE64_ALPHABET[(b0 >> 2) as usize] as char);
        out.push(BASE64_ALPHABET[(((b0 & 0b11) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(BASE64_ALPHABET[(((b1 & 0b1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(BASE64_ALPHABET[(b2 & 0b11_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_decode(input: &str) -> Result<Vec<u8>> {
    let mut decoded: Vec<u8> = Vec::with_capacity(input.len() / 4 * 3);
    let mut buf = [0u8; 4];
    let mut idx = 0usize;
    let mut padding = 0;
    for c in input.chars() {
        if c == '=' {
            padding += 1;
            buf[idx] = 0;
        } else {
            let val = BASE64_ALPHABET
                .iter()
                .position(|&b| b as char == c)
                .ok_or_else(|| SQLError::TypeMismatch(format!("invalid base64 char {c:?}")))?;
            buf[idx] = val as u8;
        }
        idx += 1;
        if idx == 4 {
            decoded.push((buf[0] << 2) | (buf[1] >> 4));
            decoded.push((buf[1] << 4) | (buf[2] >> 2));
            decoded.push((buf[2] << 6) | buf[3]);
            idx = 0;
        }
    }
    decoded.truncate(decoded.len().saturating_sub(padding));
    Ok(decoded)
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
                        return Err(SQLError::TypeMismatch(format!(
                            "vector element must be numeric, got {other:?}"
                        )))
                    }
                };
                out.push(x);
            }
            Ok(out)
        }
        other => Err(SQLError::TypeMismatch(format!(
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
        let params = vec![SQLParam::Scalar(Value::Str("hi".into()))];
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
