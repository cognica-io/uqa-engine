//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar expression evaluator: turns an [`Expr`] into a [`Value`] under
//! a row context (column -> value) and a parameter binding.

use std::borrow::Cow;

use uqa_core::{DecimalValue, TemporalValue, Value};

use crate::ast::{BinaryOp, Expr};
use crate::error::{Result, SQLError};
use crate::params::SQLParam;
use crate::result::ResultRow;

mod encoding;
mod json;
mod time;

use encoding::{base64_decode, base64_encode, md5_hex};
use json::{
    json_build_array, json_build_object, json_concat, json_contained_by, json_contains,
    json_delete, json_delete_path, json_extract_path, json_has_key, json_has_keys, json_to_value,
    json_typeof, jsonb_insert, jsonb_set, jsonpath_candidate, jsonpath_exists, jsonpath_match,
    parse_json, strip_nulls, value_to_json,
};
use time::{
    date_trunc, extract_field, format_pg_datetime, format_pg_number, generate_random_uuid,
    hex_encode, make_timestamp, parse_timestamp, pg_to_chrono_fmt,
};

/// Engine-side hook that the expression evaluator calls into for
/// stateful scalar functions and subquery execution. Keeps
/// `uqa-sql` independent of `uqa-engine` while still letting the
/// evaluator drive `nextval` / `currval` / `setval` and run
/// `(SELECT ...)` / `EXISTS (...)` / `IN (SELECT ...)` subqueries.
pub trait EngineHook {
    fn nextval(&self, name: &str) -> std::result::Result<i64, String>;
    fn currval(&self, name: &str) -> std::result::Result<i64, String>;
    fn setval(&self, name: &str, value: i64) -> std::result::Result<i64, String>;
    /// Run a subquery and return its rows + column ordering. The
    /// evaluator extracts a scalar (single-row, single-column) from
    /// the result for `ScalarSubquery`, sees whether the row count
    /// is zero for `Exists`, and tests membership for `InSubquery`.
    /// Default returns Unsupported so backends that don't surface
    /// subqueries still satisfy the trait.
    fn run_subquery(
        &self,
        _stmt: &crate::ast::SelectStmt,
        _row: Option<&crate::result::ResultRow>,
        _params: &[crate::params::SQLParam],
    ) -> std::result::Result<(Vec<String>, Vec<crate::result::ResultRow>), String> {
        Err("subquery execution not supported by this engine".into())
    }

    fn call_scalar_function(&self, _name: &str, _args: &[Value]) -> Option<Result<Value>> {
        None
    }

    fn has_scalar_functions(&self) -> bool {
        true
    }
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
            Some(SQLParam::Tensor(vectors)) => Ok(Value::List(
                vectors
                    .iter()
                    .map(|vector| {
                        Value::List(vector.iter().map(|x| Value::Float(f64::from(*x))).collect())
                    })
                    .collect(),
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
            for (key, value) in row {
                if key.rsplit_once('.').is_some_and(|(_, col)| col == name) {
                    return Ok(value.clone());
                }
            }
            Ok(Value::Null)
        }
        Expr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => {
            let row = ctx
                .row
                .ok_or_else(|| SQLError::Internal("column reference without row context".into()))?;
            if key.is_empty() {
                let key = format!("{qualifier}.{column}");
                Ok(row.get(&key).cloned().unwrap_or(Value::Null))
            } else {
                Ok(row.get(key).cloned().unwrap_or(Value::Null))
            }
        }
        Expr::Array(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for e in elements {
                out.push(eval(e, ctx)?);
            }
            Ok(Value::List(out))
        }
        Expr::Star => Err(SQLError::Internal("`*` cannot be evaluated".into())),
        Expr::Func { name, args, .. } => {
            // Functions registered in the operator registry (text_match,
            // knn_match, ...) are dispatched by the engine; only pure
            // scalar built-ins are evaluated inline here.
            let lower = normalized_function_name(name);
            let lower = lower.as_ref();
            if crate::registry::is_registered(lower) {
                if lower == "fts_match" {
                    let evaluated: Vec<Value> = args
                        .iter()
                        .map(|a| eval(a, ctx))
                        .collect::<Result<Vec<_>>>()?;
                    if jsonpath_candidate(&evaluated) {
                        return jsonpath_match(&evaluated);
                    }
                }
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
            // canonical UQA behavior). Routed before the pure-scalar
            // dispatch so they take precedence over any future
            // built-in named the same.
            if matches!(lower, "nextval" | "currval" | "setval") {
                return eval_sequence_function(lower, &evaluated, ctx);
            }
            if let Some(engine) = ctx.engine.filter(|engine| engine.has_scalar_functions()) {
                if let Some(result) = engine.call_scalar_function(lower, &evaluated) {
                    return result;
                }
            }
            eval_scalar_function(lower, &evaluated)
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
        Expr::ScalarSubquery(body) => {
            let engine = ctx.engine.ok_or_else(|| {
                SQLError::Unsupported(
                    "scalar subquery requires an engine hook on the EvalContext".into(),
                )
            })?;
            let (cols, rows) = engine
                .run_subquery(body, ctx.row, ctx.params)
                .map_err(SQLError::Unsupported)?;
            if rows.is_empty() {
                return Ok(Value::Null);
            }
            if rows.len() > 1 {
                return Err(SQLError::TypeMismatch(
                    "scalar subquery returned more than one row".into(),
                ));
            }
            let first_col = cols.first().ok_or_else(|| {
                SQLError::TypeMismatch("scalar subquery returned no columns".into())
            })?;
            Ok(rows[0].get(first_col).cloned().unwrap_or(Value::Null))
        }
        Expr::Exists { body, negated } => {
            let engine = ctx.engine.ok_or_else(|| {
                SQLError::Unsupported(
                    "EXISTS subquery requires an engine hook on the EvalContext".into(),
                )
            })?;
            let (_cols, rows) = engine
                .run_subquery(body, ctx.row, ctx.params)
                .map_err(SQLError::Unsupported)?;
            let exists = !rows.is_empty();
            Ok(Value::Bool(if *negated { !exists } else { exists }))
        }
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => {
            let needle = eval(expr, ctx)?;
            let engine = ctx.engine.ok_or_else(|| {
                SQLError::Unsupported(
                    "IN (SELECT ...) requires an engine hook on the EvalContext".into(),
                )
            })?;
            let (cols, rows) = engine
                .run_subquery(body, ctx.row, ctx.params)
                .map_err(SQLError::Unsupported)?;
            let Some(first_col) = cols.first() else {
                return Ok(Value::Bool(*negated));
            };
            let found = rows
                .iter()
                .any(|r| r.get(first_col).is_some_and(|v| v == &needle));
            Ok(Value::Bool(if *negated { !found } else { found }))
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

fn normalized_function_name(name: &str) -> Cow<'_, str> {
    let stripped = name.strip_prefix("pg_catalog.").unwrap_or(name);
    if stripped.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(stripped.to_ascii_lowercase())
    } else {
        Cow::Borrowed(stripped)
    }
}

fn eval_binary(op: BinaryOp, lhs: &Expr, rhs: &Expr, ctx: &EvalContext<'_>) -> Result<Value> {
    if let Some(value) = eval_binary_borrowed(op, lhs, rhs, ctx)? {
        return Ok(value);
    }
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

enum EvalOperand<'a> {
    Borrowed(&'a Value),
    Owned(Value),
}

impl EvalOperand<'_> {
    fn as_value(&self) -> &Value {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

fn eval_binary_borrowed(
    op: BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    ctx: &EvalContext<'_>,
) -> Result<Option<Value>> {
    if !matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        return Ok(None);
    }
    let Some(l) = eval_operand_borrowed(lhs, ctx)? else {
        return Ok(None);
    };
    let Some(r) = eval_operand_borrowed(rhs, ctx)? else {
        return Ok(None);
    };
    let l = l.as_value();
    let r = r.as_value();
    let out = match op {
        BinaryOp::Equal => Value::Bool(values_equal(l, r)),
        BinaryOp::NotEqual => Value::Bool(!values_equal(l, r)),
        BinaryOp::Less => Value::Bool(compare(l, r)?.is_lt()),
        BinaryOp::LessEqual => Value::Bool(compare(l, r)?.is_le()),
        BinaryOp::Greater => Value::Bool(compare(l, r)?.is_gt()),
        BinaryOp::GreaterEqual => Value::Bool(compare(l, r)?.is_ge()),
        _ => unreachable!("non-comparison op filtered above"),
    };
    Ok(Some(out))
}

fn eval_operand_borrowed<'a>(
    expr: &Expr,
    ctx: &EvalContext<'a>,
) -> Result<Option<EvalOperand<'a>>> {
    match expr {
        Expr::Literal(value) => Ok(Some(EvalOperand::Owned(value.clone()))),
        Expr::Param(i) => match ctx.params.get(i.saturating_sub(1)) {
            Some(SQLParam::Scalar(value)) => Ok(Some(EvalOperand::Borrowed(value))),
            Some(SQLParam::Vector(_)) | Some(SQLParam::Tensor(_)) => Ok(None),
            None => Err(SQLError::MissingParam(*i)),
        },
        Expr::Column(name) => {
            let row = ctx
                .row
                .ok_or_else(|| SQLError::Internal("column reference without row context".into()))?;
            Ok(Some(match row_column_value(row, name) {
                Some(value) => EvalOperand::Borrowed(value),
                None => EvalOperand::Owned(Value::Null),
            }))
        }
        Expr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => {
            let row = ctx
                .row
                .ok_or_else(|| SQLError::Internal("column reference without row context".into()))?;
            let value = if key.is_empty() {
                let key = format!("{qualifier}.{column}");
                row.get(&key)
            } else {
                row.get(key)
            };
            Ok(Some(match value {
                Some(value) => EvalOperand::Borrowed(value),
                None => EvalOperand::Owned(Value::Null),
            }))
        }
        _ => Ok(None),
    }
}

fn row_column_value<'a>(row: &'a ResultRow, name: &str) -> Option<&'a Value> {
    if let Some(value) = row.get(name) {
        return Some(value);
    }
    row.iter()
        .find(|(key, _)| key.rsplit_once('.').is_some_and(|(_, col)| col == name))
        .map(|(_, value)| value)
}

/// `NULL` is falsy; otherwise truthy iff the value coerces to a non-zero
/// boolean / number / non-empty string.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::Decimal(d) => !d.is_zero(),
        Value::Str(s) => !s.is_empty(),
        _ => true,
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        (Value::Decimal(x), Value::Decimal(y)) => x == y,
        (Value::Int(x), Value::Decimal(y)) | (Value::Decimal(y), Value::Int(x)) => {
            DecimalValue::from_i64(*x) == *y
        }
        (Value::Float(x), Value::Decimal(y)) | (Value::Decimal(y), Value::Float(x)) => {
            DecimalValue::from_f64_lossy(*x).is_some_and(|x| x == *y)
        }
        (Value::Bool(x), Value::Decimal(y)) | (Value::Decimal(y), Value::Bool(x)) => {
            DecimalValue::from_bool(*x) == *y
        }
        (Value::Temporal(x), Value::Temporal(y)) => x == y,
        (Value::Temporal(x), Value::Str(y)) | (Value::Str(y), Value::Temporal(x)) => {
            x.parse_same_kind(y).is_some_and(|parsed| parsed == *x)
        }
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
        (Value::Decimal(x), Value::Decimal(y)) => Ok(x.cmp(y)),
        (Value::Int(x), Value::Decimal(y)) => Ok(DecimalValue::from_i64(*x).cmp(y)),
        (Value::Decimal(x), Value::Int(y)) => Ok(x.cmp(&DecimalValue::from_i64(*y))),
        (Value::Float(x), Value::Decimal(y)) => DecimalValue::from_f64_lossy(*x)
            .map(|x| x.cmp(y))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Decimal(x), Value::Float(y)) => DecimalValue::from_f64_lossy(*y)
            .map(|y| x.cmp(&y))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Bool(x), Value::Decimal(y)) => Ok(DecimalValue::from_bool(*x).cmp(y)),
        (Value::Decimal(x), Value::Bool(y)) => Ok(x.cmp(&DecimalValue::from_bool(*y))),
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y)),
        (Value::Temporal(x), Value::Temporal(y)) => Ok(x.cmp(y)),
        (Value::Temporal(x), Value::Str(y)) => x
            .parse_same_kind(y)
            .map(|parsed| x.cmp(&parsed))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Str(x), Value::Temporal(y)) => y
            .parse_same_kind(x)
            .map(|parsed| parsed.cmp(y))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        (lhs, rhs) => Err(SQLError::TypeMismatch(format!(
            "cannot compare {lhs:?} with {rhs:?}"
        ))),
    }
}

fn arith(a: &Value, b: &Value, op: BinaryOp) -> Result<Value> {
    // SQL three-valued logic: NULL `op` anything == NULL.
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return Ok(Value::Null);
    }
    if matches!(op, BinaryOp::Subtract) {
        if let Some(value) = json_delete(&[a.clone(), b.clone()])? {
            return Ok(value);
        }
    }
    if matches!(a, Value::Decimal(_)) || matches!(b, Value::Decimal(_)) {
        return decimal_arith(a, b, op);
    }
    let lf = to_f64(a)?;
    let rf = to_f64(b)?;
    let result = match op {
        BinaryOp::Add => lf + rf,
        BinaryOp::Subtract => lf - rf,
        BinaryOp::Multiply => lf * rf,
        BinaryOp::Divide => {
            // UQA expression evaluation surfaces division by zero as NULL
            // so SQL row evaluation does not fail mid-projection.
            if rf == 0.0 {
                return Ok(Value::Null);
            }
            // Integer / Integer in SQL truncates toward zero.
            if matches!((a, b), (Value::Int(_), Value::Int(_))) {
                let li = match a {
                    Value::Int(n) => *n,
                    _ => unreachable!(),
                };
                let ri = match b {
                    Value::Int(n) => *n,
                    _ => unreachable!(),
                };
                return Ok(Value::Int(li / ri));
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

fn decimal_arith(a: &Value, b: &Value, op: BinaryOp) -> Result<Value> {
    let left = to_decimal(a)?;
    let right = to_decimal(b)?;
    let value = match op {
        BinaryOp::Add => left.checked_add(&right),
        BinaryOp::Subtract => left.checked_sub(&right),
        BinaryOp::Multiply => left.checked_mul(&right),
        BinaryOp::Divide => {
            if right.is_zero() {
                return Ok(Value::Null);
            }
            left.checked_div(&right)
        }
        _ => unreachable!("non-arith op routed through decimal_arith"),
    }
    .ok_or_else(|| SQLError::TypeMismatch("decimal arithmetic overflow".into()))?;
    Ok(Value::Decimal(value))
}

// -------------------------------------------------------------------------
// Built-in scalar functions
// -------------------------------------------------------------------------

/// Dispatch table for built-in scalar SQL functions. Mirrors
/// `_call_scalar_function` in UQA `sql/expr_evaluator`. Function
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
        "trim" | "btrim" => string1(args, |s| s.trim().to_string()),
        "ltrim" => string1(args, |s| s.trim_start().to_string()),
        "rtrim" => string1(args, |s| s.trim_end().to_string()),
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
            // SUBSTRING(string, start [, length]). 1-indexed per SQL.
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
            if args.iter().any(|arg| matches!(arg, Value::Null)) {
                return Ok(Value::Null);
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
            if args.iter().any(|arg| matches!(arg, Value::Null)) {
                return Ok(Value::Null);
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
            Value::Decimal(d) => Ok(Value::Decimal(d.abs())),
            Value::Null => Ok(Value::Null),
            other => Err(SQLError::TypeMismatch(format!(
                "abs() expected number, got {other:?}"
            ))),
        },
        "round" => match args.len() {
            1 => match &args[0] {
                Value::Int(i) => Ok(Value::Int(*i)),
                Value::Float(f) => Ok(Value::Float(f.round())),
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
                        .ok_or_else(|| SQLError::TypeMismatch("decimal round overflow".into()));
                }
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
            match (&args[0], &args[1]) {
                (Value::Int(a), Value::Int(b)) if *b != 0 => Ok(Value::Int(a % b)),
                (a, b) if matches!(a, Value::Decimal(_)) || matches!(b, Value::Decimal(_)) => {
                    let divisor = to_decimal(b)?;
                    if divisor.is_zero() {
                        Err(SQLError::TypeMismatch("modulo by zero".into()))
                    } else {
                        to_decimal(a)?
                            .checked_rem(&divisor)
                            .map(Value::Decimal)
                            .ok_or_else(|| SQLError::TypeMismatch("decimal modulo overflow".into()))
                    }
                }
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
        "div" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("div takes 2 args".into()));
            }
            let divisor = to_i64(&args[1])?;
            if divisor == 0 {
                return Err(SQLError::TypeMismatch("division by zero".into()));
            }
            let dividend = to_i64(&args[0])?;
            Ok(Value::Int((dividend as f64 / divisor as f64).floor() as i64))
        }
        "gcd" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("gcd takes 2 args".into()));
            }
            Ok(Value::Int(gcd_i64(to_i64(&args[0])?, to_i64(&args[1])?)))
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
                Ok(Value::Int((a / gcd_i64(a, b)).abs() * b.abs()))
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
        "sin" => float1(args, "sin", f64::sin),
        "cos" => float1(args, "cos", f64::cos),
        "tan" => float1(args, "tan", f64::tan),
        "asin" => float1(args, "asin", f64::asin),
        "acos" => float1(args, "acos", f64::acos),
        "atan" => float1(args, "atan", f64::atan),
        "atan2" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("atan2 takes 2 args".into()));
            }
            if args.iter().any(|arg| matches!(arg, Value::Null)) {
                return Ok(Value::Null);
            }
            Ok(Value::Float(to_f64(&args[0])?.atan2(to_f64(&args[1])?)))
        }
        "sinh" => float1(args, "sinh", f64::sinh),
        "cosh" => float1(args, "cosh", f64::cosh),
        "tanh" => float1(args, "tanh", f64::tanh),
        "exp" => float1(args, "exp", f64::exp),
        "ln" => float1(args, "ln", f64::ln),
        "log" | "log10" => match args.len() {
            1 => float1(args, "log", f64::log10),
            2 => {
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let base = to_f64(&args[0])?;
                let v = to_f64(&args[1])?;
                Ok(Value::Float(v.log(base)))
            }
            _ => Err(SQLError::TypeMismatch("log takes 1 or 2 args".into())),
        },
        "log2" => float1(args, "log2", f64::log2),
        "cbrt" => float1(args, "cbrt", f64::cbrt),
        "sign" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("sign takes 1 arg".into()));
            }
            if matches!(args[0], Value::Null) {
                return Ok(Value::Null);
            }
            Ok(Value::Int(match to_f64(&args[0])? {
                v if v > 0.0 => 1,
                v if v < 0.0 => -1,
                _ => 0,
            }))
        }
        "trunc" => match args.len() {
            1 => match &args[0] {
                Value::Int(i) => Ok(Value::Int(*i)),
                Value::Float(f) => Ok(Value::Float(f.trunc())),
                Value::Decimal(d) => Ok(Value::Decimal(d.trunc())),
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!("trunc({other:?})"))),
            },
            2 => {
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                if matches!(args[0], Value::Decimal(_)) {
                    let places = to_i64(&args[1])?;
                    let places = i32::try_from(places).map_err(|_| {
                        SQLError::TypeMismatch(format!("trunc scale out of range: {places}"))
                    })?;
                    return to_decimal(&args[0])?
                        .trunc_to_scale(places)
                        .map(Value::Decimal)
                        .ok_or_else(|| SQLError::TypeMismatch("decimal trunc overflow".into()));
                }
                let v = to_f64(&args[0])?;
                let p = to_i64(&args[1])?;
                let scale = 10f64.powi(p as i32);
                Ok(Value::Float((v * scale).trunc() / scale))
            }
            _ => Err(SQLError::TypeMismatch("trunc takes 1 or 2 args".into())),
        },
        "pi" => Ok(Value::Float(std::f64::consts::PI)),
        "degrees" => float1(args, "degrees", f64::to_degrees),
        "radians" => float1(args, "radians", f64::to_radians),
        "random" => {
            // Deterministic-ish pseudo random based on system time so
            // tests can assert ranges deterministically; the canonical
            // UQA behavior also wraps the platform RNG.
            use std::time::{SystemTime, UNIX_EPOCH};
            let t = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0) as f64;
            Ok(Value::Float((t.sin().abs() * 1.0e9).fract()))
        }
        "width_bucket" => {
            if args.len() != 4 {
                return Err(SQLError::TypeMismatch("width_bucket takes 4 args".into()));
            }
            let operand = to_f64(&args[0])?;
            let low = to_f64(&args[1])?;
            let high = to_f64(&args[2])?;
            let count = to_i64(&args[3])?;
            if count <= 0 || low == high {
                return Err(SQLError::TypeMismatch(
                    "width_bucket requires positive bucket count and non-empty range".into(),
                ));
            }
            if low < high {
                if operand < low {
                    return Ok(Value::Int(0));
                }
                if operand >= high {
                    return Ok(Value::Int(count + 1));
                }
                let width = (high - low) / count as f64;
                Ok(Value::Int(((operand - low) / width).floor() as i64 + 1))
            } else {
                if operand > low {
                    return Ok(Value::Int(0));
                }
                if operand <= high {
                    return Ok(Value::Int(count + 1));
                }
                let width = (low - high) / count as f64;
                Ok(Value::Int(((low - operand) / width).floor() as i64 + 1))
            }
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
        "md5" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("md5 takes 1 arg".into()));
            }
            Ok(Value::Str(md5_hex(value_to_string(&args[0]).as_bytes())))
        }
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
            // make_interval(years, months, weeks, days, hours, mins, secs).
            // Mirrors the canonical UQA behavior's compact HH:MM:SS interval
            // representation, with years/months normalized to days.
            let years = args.first().map(to_i64).transpose()?.unwrap_or(0);
            let months = args.get(1).map(to_i64).transpose()?.unwrap_or(0);
            let weeks = args.get(2).map(to_i64).transpose()?.unwrap_or(0);
            let days = args.get(3).map(to_i64).transpose()?.unwrap_or(0);
            let hours = args.get(4).map(to_i64).transpose()?.unwrap_or(0);
            let mins = args.get(5).map(to_i64).transpose()?.unwrap_or(0);
            let secs = args.get(6).map(to_f64).transpose()?.unwrap_or(0.0);
            let total_days = years * 365 + months * 30 + weeks * 7 + days;
            let total_seconds =
                total_days * 86_400 + hours * 3_600 + mins * 60 + secs.trunc() as i64;
            let h_part = total_seconds / 3_600;
            let m_part = (total_seconds % 3_600) / 60;
            let s_part = total_seconds % 60;
            Ok(Value::Str(format!("{h_part:02}:{m_part:02}:{s_part:02}")))
        }
        "to_char" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("to_char takes 2 args".into()));
            }
            let fmt = value_to_string(&args[1]);
            match &args[0] {
                Value::Int(i) => Ok(Value::Str(format_pg_number(*i as f64, &fmt))),
                Value::Float(f) => Ok(Value::Str(format_pg_number(*f, &fmt))),
                Value::Decimal(d) => d
                    .to_f64()
                    .map(|value| Value::Str(format_pg_number(value, &fmt)))
                    .ok_or_else(|| SQLError::TypeMismatch("to_char: numeric out of range".into())),
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
            DecimalValue::parse(&cleaned)
                .map(Value::Decimal)
                .ok_or_else(|| SQLError::TypeMismatch(format!("to_number: {s:?}")))
        }
        "isfinite" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("isfinite takes 1 arg".into()));
            }
            match &args[0] {
                Value::Float(f) => Ok(Value::Bool(f.is_finite())),
                Value::Int(_) | Value::Decimal(_) | Value::Str(_) => Ok(Value::Bool(true)),
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
        "json_build_array" | "jsonb_build_array" => Ok(json_build_array(args)),
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
        "json_contains" => json_contains(args),
        "json_contained_by" => json_contained_by(args),
        "json_delete_path" => json_delete_path(args),
        "json_has_key" => json_has_key(args),
        "json_has_any_key" => json_has_keys(args, false),
        "json_has_all_keys" => json_has_keys(args, true),
        "jsonb_path_exists" | "jsonpath_exists" => jsonpath_exists(args),
        "jsonb_path_match" | "jsonpath_match" => jsonpath_match(args),
        "to_json" | "to_jsonb" | "row_to_json" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("to_json takes 1 arg".into()));
            }
            let json = value_to_json(&args[0]);
            Ok(json_to_value(&json))
        }
        "jsonb_set" => jsonb_set(args),
        "jsonb_insert" => jsonb_insert(args),
        "jsonb_pretty" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("jsonb_pretty takes 1 arg".into()));
            }
            let parsed = parse_json(&value_to_string(&args[0]))?;
            Ok(Value::Str(serde_json::to_string_pretty(&parsed).map_err(
                |err| SQLError::TypeMismatch(format!("jsonb_pretty: {err}")),
            )?))
        }
        "json_strip_nulls" | "jsonb_strip_nulls" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch(
                    "json_strip_nulls takes 1 arg".into(),
                ));
            }
            let mut parsed = parse_json(&value_to_string(&args[0]))?;
            strip_nulls(&mut parsed);
            Ok(json_to_value(&parsed))
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
            // `st_dwithin` uses the Euclidean radius semantics supported by
            // this scalar evaluator. Polygon containment is handled by the
            // spatial operator layer rather than this value-only function.
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

fn typeof_value(v: &Value) -> String {
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
        },
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
    if let Some(elem_ty) = ty.strip_suffix("[]") {
        let Value::List(items) = v else {
            return Err(SQLError::TypeMismatch(format!(
                "CAST AS {ty}: expected array, got {v:?}"
            )));
        };
        return items
            .iter()
            .map(|item| cast_value(item, elem_ty))
            .collect::<Result<Vec<_>>>()
            .map(Value::List);
    }
    match ty {
        "integer" | "int" | "int2" | "int4" | "int8" | "bigint" | "smallint" | "serial"
        | "bigserial" | "serial4" | "serial8" | "pg_catalog.int4" | "pg_catalog.int8"
        | "pg_catalog.int2" => Ok(Value::Int(to_i64(v)?)),
        "real" | "float4" | "float8" | "double" | "double precision" => {
            Ok(Value::Float(to_f64(v)?))
        }
        "numeric" | "decimal" => Ok(Value::Decimal(to_decimal(v)?)),
        "text" | "varchar" | "character" | "char" | "bpchar" | "name" | "uuid" => {
            Ok(Value::Str(value_to_string(v)))
        }
        "date" => cast_temporal(v, TemporalValue::parse_date, "date"),
        "time" | "time without time zone" => cast_temporal(v, TemporalValue::parse_time, "time"),
        "timetz" | "time with time zone" => {
            cast_temporal(v, TemporalValue::parse_time_tz, "time with time zone")
        }
        "timestamp" | "datetime" | "timestamp without time zone" => {
            cast_temporal(v, TemporalValue::parse_timestamp, "timestamp")
        }
        "timestamptz" | "timestamp with time zone" => cast_temporal(
            v,
            TemporalValue::parse_timestamp_tz,
            "timestamp with time zone",
        ),
        "json" | "jsonb" => Ok(json_to_value(&parse_json(&value_to_string(v))?)),
        "bytea" => match v {
            Value::Bytes(bytes) => Ok(Value::Bytes(bytes.clone())),
            Value::Str(s) => Ok(Value::Bytes(s.as_bytes().to_vec())),
            other => Ok(Value::Bytes(value_to_string(other).into_bytes())),
        },
        "boolean" | "bool" => Ok(Value::Bool(truthy(v))),
        other => Err(SQLError::Unsupported(format!("CAST AS {other}"))),
    }
}

fn cast_temporal(v: &Value, parse: fn(&str) -> Option<TemporalValue>, ty: &str) -> Result<Value> {
    match v {
        Value::Temporal(value) => Ok(Value::Temporal(value.clone())),
        other => parse(&value_to_string(other))
            .map(Value::Temporal)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to {ty}"))),
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => "".into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) => s.clone(),
        Value::Bool(b) => (if *b { "true" } else { "false" }).into(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::List(_) | Value::Map(_) => {
            serde_json::to_string(&value_to_json(v)).unwrap_or_default()
        }
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
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

fn float1<F: FnOnce(f64) -> f64>(args: &[Value], name: &str, f: F) -> Result<Value> {
    if args.len() != 1 {
        return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    Ok(Value::Float(f(to_f64(&args[0])?)))
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
        Value::Decimal(d) => d
            .to_i64_trunc()
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to integer"))),
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
        Value::Decimal(d) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch(format!("cannot cast {v:?} to double precision"))
        }),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

fn to_decimal(v: &Value) -> Result<DecimalValue> {
    match v {
        Value::Decimal(d) => Ok(d.clone()),
        Value::Int(n) => Ok(DecimalValue::from_i64(*n)),
        Value::Float(f) => DecimalValue::from_f64_lossy(*f)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to numeric"))),
        Value::Bool(b) => Ok(DecimalValue::from_bool(*b)),
        Value::Str(s) => DecimalValue::parse(s)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot parse {s:?} as numeric"))),
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Best-effort `Value -> i64`. Returns `None` for shapes that do not
/// have a well-defined integer projection (e.g. `Value::Null`).
fn coerce_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        Value::Float(f) => Some(*f as i64),
        Value::Decimal(d) => d.to_i64_trunc(),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Str(s) => s.parse().ok(),
        _ => None,
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
                    Value::Decimal(d) => d.to_f64().map(|f| f as f32).ok_or_else(|| {
                        SQLError::TypeMismatch(format!("vector element must fit f32, got {item:?}"))
                    })?,
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

/// Coerce a [`Value`] into a tensor: an array of homogeneous numeric
/// vectors. Used by `TENSOR(N)` columns to store chunk embeddings for one
/// row while still indexing each vector element.
pub fn value_to_tensor(v: &Value) -> Result<Vec<Vec<f32>>> {
    match v {
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(value_to_vector(item)?);
            }
            Ok(out)
        }
        other => Err(SQLError::TypeMismatch(format!(
            "expected tensor (list of numeric lists), got {other:?}"
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

    #[test]
    fn value_to_tensor_accepts_array_of_vectors() {
        let v = Value::List(vec![
            Value::List(vec![Value::Float(1.0), Value::Int(0)]),
            Value::List(vec![Value::Int(0), Value::Float(1.0)]),
        ]);
        let got = value_to_tensor(&v).unwrap();
        assert_eq!(got, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }
}
