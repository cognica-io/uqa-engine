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
    age_between, coerce_temporal, date_trunc_value, extract_from_value, format_pg_number,
    format_temporal, generate_random_uuid, hex_encode, make_timestamp, parse_timestamp,
    pg_to_chrono_fmt,
};

/// Engine-side hook that scalar function evaluation calls for stateful
/// sequence and user-defined functions. Query-valued expressions are not
/// accepted here: lowering assigns them physical query-plan slots executed by
/// `uqa-execution::ScalarSubqueryRunner`.
pub trait EngineHook {
    fn nextval(&self, name: &str) -> std::result::Result<i64, String>;
    fn currval(&self, name: &str) -> std::result::Result<i64, String>;
    fn setval(&self, name: &str, value: i64) -> std::result::Result<i64, String>;

    fn call_scalar_function(&self, _name: &str, _args: &[Value]) -> Option<Result<Value>> {
        None
    }

    fn has_scalar_functions(&self) -> bool {
        true
    }

    /// Invoke a user-defined SQL / `PL/pgSQL` function. Consulted
    /// after built-in dispatch misses (and immediately for calls with
    /// named arguments, which built-ins never accept). `None` means
    /// no user-defined function with this name exists.
    fn call_user_function(
        &self,
        _name: &str,
        _args: &[(Option<String>, Value)],
    ) -> Option<Result<Value>> {
        None
    }
}

/// Read-only row interface used by the expression evaluator. Most callers
/// use a materialised [`ResultRow`], while hot execution paths can expose a
/// projected value slice without rebuilding a string-keyed map for every row.
pub trait RowLookup {
    fn column(&self, name: &str) -> Option<&Value>;

    fn qualified_column(&self, qualifier: &str, column: &str, key: &str) -> Option<&Value>;

    /// Physical correlated subqueries require the concrete outer row passed
    /// to `ScalarSubqueryRunner`. Projected row views return `None`; planners
    /// only use them for expressions that cannot contain subqueries.
    fn result_row(&self) -> Option<&ResultRow> {
        None
    }
}

impl RowLookup for ResultRow {
    fn column(&self, name: &str) -> Option<&Value> {
        row_column_value(self, name)
    }

    fn qualified_column(&self, qualifier: &str, column: &str, key: &str) -> Option<&Value> {
        let value = if key.is_empty() {
            let qualified_key = format!("{qualifier}.{column}");
            self.get(&qualified_key)
        } else {
            self.get(key)
        };
        value.or_else(|| unqualified_fallback(self, column))
    }

    fn result_row(&self) -> Option<&ResultRow> {
        Some(self)
    }
}

pub struct EvalContext<'a> {
    pub row: Option<&'a ResultRow>,
    row_lookup: Option<&'a dyn RowLookup>,
    pub params: &'a [SQLParam],
    pub engine: Option<&'a dyn EngineHook>,
}

impl<'a> EvalContext<'a> {
    pub fn new(row: Option<&'a ResultRow>, params: &'a [SQLParam]) -> Self {
        Self {
            row,
            row_lookup: row.map(|row| row as &dyn RowLookup),
            params,
            engine: None,
        }
    }

    pub fn from_row_lookup(row: &'a dyn RowLookup, params: &'a [SQLParam]) -> Self {
        Self {
            row: row.result_row(),
            row_lookup: Some(row),
            params,
            engine: None,
        }
    }

    pub fn with_engine(mut self, engine: &'a dyn EngineHook) -> Self {
        self.engine = Some(engine);
        self
    }

    fn row_lookup(&self) -> Result<&'a dyn RowLookup> {
        self.row_lookup
            .ok_or_else(|| SQLError::Internal("column reference without row context".into()))
    }

    /// Resolve an unqualified column through the same row semantics used by
    /// the AST evaluator. Physical scalar IR evaluators call this instead of
    /// reconstructing an [`Expr::Column`] carrier.
    pub fn column_value(&self, name: &str) -> Result<Value> {
        Ok(self
            .row_lookup()?
            .column(name)
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Resolve a qualified column without constructing an AST expression.
    pub fn qualified_column_value(
        &self,
        qualifier: &str,
        column: &str,
        key: &str,
    ) -> Result<Value> {
        Ok(self
            .row_lookup()?
            .qualified_column(qualifier, column, key)
            .cloned()
            .unwrap_or(Value::Null))
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
            // Plain column refs match either an unqualified key or the
            // suffix of a qualified `table.col` key, so the same row
            // shape works for single-table SELECTs and JOIN tuples.
            Ok(ctx
                .row_lookup()?
                .column(name)
                .cloned()
                .unwrap_or(Value::Null))
        }
        Expr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => Ok(ctx
            .row_lookup()?
            .qualified_column(qualifier, column, key)
            .cloned()
            .unwrap_or(Value::Null)),
        Expr::Array(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for e in elements {
                out.push(eval(e, ctx)?);
            }
            Ok(Value::List(out))
        }
        Expr::Star => Err(SQLError::Internal("`*` cannot be evaluated".into())),
        Expr::Func { name, args, .. } => {
            let call_args = evaluate_call_args(args, ctx)?;
            eval_function_call(name, call_args, ctx)
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
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
            Err(SQLError::Unsupported(
                "query-valued expressions must be lowered to physical ScalarExpr/QueryPlan slots"
                    .into(),
            ))
        }
        Expr::Binary { op, lhs, rhs } => eval_binary(*op, lhs, rhs, ctx),
        Expr::Not(inner) => {
            // SQL three-valued logic: NOT NULL -> NULL.
            let v = eval(inner, ctx)?;
            if matches!(v, Value::Null) {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(!truthy(&v)))
        }
        Expr::And(items) => {
            // Kleene AND: FALSE dominates, otherwise NULL taints.
            let mut saw_null = false;
            for item in items {
                let v = eval(item, ctx)?;
                if matches!(v, Value::Null) {
                    saw_null = true;
                } else if !truthy(&v) {
                    return Ok(Value::Bool(false));
                }
            }
            if saw_null {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(true))
        }
        Expr::Or(items) => {
            // Kleene OR: TRUE dominates, otherwise NULL taints.
            let mut saw_null = false;
            for item in items {
                let v = eval(item, ctx)?;
                if matches!(v, Value::Null) {
                    saw_null = true;
                } else if truthy(&v) {
                    return Ok(Value::Bool(true));
                }
            }
            if saw_null {
                return Ok(Value::Null);
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
            eval_between(&v, &lo, &hi)
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            // Three-valued IN: found -> TRUE, a NULL comparand (or a
            // NULL needle) downgrades a miss to NULL.
            let v = eval(expr, ctx)?;
            let mut saw_null = matches!(v, Value::Null);
            for item in list {
                let candidate = eval(item, ctx)?;
                match values_equal_nullable(&v, &candidate) {
                    Some(true) => return Ok(Value::Bool(!*negated)),
                    Some(false) => {}
                    None => saw_null = true,
                }
            }
            if saw_null {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(*negated))
        }
    }
}

/// `expr BETWEEN low AND high` under three-valued logic: a definite
/// FALSE on either bound wins over a NULL on the other.
fn eval_between(v: &Value, lo: &Value, hi: &Value) -> Result<Value> {
    let ge = compare_nullable(v, lo)?.map(|ord| ord.is_ge());
    let le = compare_nullable(v, hi)?.map(|ord| ord.is_le());
    Ok(match (ge, le) {
        (Some(false), _) | (_, Some(false)) => Value::Bool(false),
        (Some(true), Some(true)) => Value::Bool(true),
        _ => Value::Null,
    })
}

/// Fallback for a qualified column reference against a row keyed by
/// bare column names (single-relation result rows). Declines when a
/// different qualifier owns the column so join rows never
/// mis-resolve.
pub fn unqualified_fallback<'a>(row: &'a ResultRow, column: &str) -> Option<&'a Value> {
    let claimed_by_other = row.keys().any(|key| {
        key.rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix == column)
    });
    if claimed_by_other {
        return None;
    }
    row.get(column)
}

fn normalized_function_name(name: &str) -> Cow<'_, str> {
    let stripped = name.strip_prefix("pg_catalog.").unwrap_or(name);
    if stripped.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(stripped.to_ascii_lowercase())
    } else {
        Cow::Borrowed(stripped)
    }
}

/// Marker function the compiler wraps `name => value` call arguments
/// in (`NamedArgExpr` has no dedicated AST node).
pub const NAMED_ARG_FUNCTION: &str = "__named_arg";

/// Evaluate a call's argument list, unwrapping `name => value`
/// markers into `(Some(name), value)` pairs.
pub fn evaluate_call_args(
    args: &[Expr],
    ctx: &EvalContext<'_>,
) -> Result<Vec<(Option<String>, Value)>> {
    args.iter()
        .map(|arg| match arg {
            Expr::Func {
                name, args: inner, ..
            } if name == NAMED_ARG_FUNCTION => {
                let Some(Expr::Literal(Value::Str(arg_name))) = inner.first() else {
                    return Err(SQLError::Internal("named argument without a name".into()));
                };
                let value_expr = inner
                    .get(1)
                    .ok_or_else(|| SQLError::Internal("named argument without a value".into()))?;
                Ok((Some(arg_name.to_ascii_lowercase()), eval(value_expr, ctx)?))
            }
            other => Ok((None, eval(other, ctx)?)),
        })
        .collect()
}

/// Execute a scalar function after its argument expressions have already
/// been evaluated.
///
/// This is the shared SQL-semantics kernel used by both the parser AST
/// evaluator and the physical scalar IR evaluator. Keeping dispatch here
/// avoids converting a physical expression back into [`Expr`] merely to
/// reuse built-in, sequence, registered, or user-defined function behavior.
pub fn eval_function_call(
    name: &str,
    call_args: Vec<(Option<String>, Value)>,
    ctx: &EvalContext<'_>,
) -> Result<Value> {
    let lower = normalized_function_name(name);
    let lower = lower.as_ref();
    let evaluated: Vec<Value> = call_args.iter().map(|(_, value)| value.clone()).collect();

    // Functions registered in the operator registry (text_match,
    // knn_match, ...) are dispatched by the relational/access-path
    // executor. JSONPath fts_match is the scalar exception.
    if crate::registry::is_registered(lower) {
        if lower == "fts_match" && jsonpath_candidate(&evaluated) {
            return jsonpath_match(&evaluated);
        }
        return Err(SQLError::Unsupported(format!(
            "scalar evaluation of `{name}` is not supported (use the function registry)"
        )));
    }

    if call_args.iter().any(|(name, _)| name.is_some()) {
        // `make_interval` is the one built-in that accepts named
        // arguments; PostgreSQL declares defaults for every parameter.
        if lower == "make_interval" {
            if let Some(positional) = make_interval_named_args(&call_args) {
                return eval_scalar_function(lower, &positional);
            }
        }
        if let Some(engine) = ctx.engine {
            if let Some(result) = engine.call_user_function(lower, &call_args) {
                return result;
            }
        }
        return Err(unknown_function_error(lower, &call_args));
    }

    // Sequence functions mutate engine state and therefore precede pure
    // built-in dispatch.
    if matches!(lower, "nextval" | "currval" | "setval") {
        return eval_sequence_function(lower, &evaluated, ctx);
    }
    if let Some(engine) = ctx.engine.filter(|engine| engine.has_scalar_functions()) {
        if let Some(result) = engine.call_scalar_function(lower, &evaluated) {
            return result;
        }
    }
    match eval_scalar_function(lower, &evaluated) {
        // Unknown built-in: fall through to user-defined functions,
        // mirroring PostgreSQL's search-path order.
        Err(SQLError::UnknownFunction(_)) => {
            if let Some(engine) = ctx.engine {
                if let Some(result) = engine.call_user_function(lower, &call_args) {
                    return result;
                }
            }
            Err(unknown_function_error(lower, &call_args))
        }
        other => other,
    }
}

/// Map `make_interval(name => value, ...)` onto the positional
/// `(years, months, weeks, days, hours, mins, secs)` argument list.
/// Returns `None` when an unknown parameter name appears.
fn make_interval_named_args(call_args: &[(Option<String>, Value)]) -> Option<Vec<Value>> {
    const NAMES: [&str; 7] = ["years", "months", "weeks", "days", "hours", "mins", "secs"];
    let mut positional = vec![Value::Int(0); NAMES.len()];
    for (idx, (name, value)) in call_args.iter().enumerate() {
        let slot = match name {
            Some(name) => NAMES.iter().position(|n| n == name)?,
            None => idx,
        };
        positional[slot] = value.clone();
    }
    Some(positional)
}

/// `PostgreSQL`-style type name used in function-resolution errors.
pub fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "unknown",
        Value::Bool(_) => "boolean",
        Value::Int(_) => "integer",
        Value::Float(_) => "double precision",
        Value::Str(_) => "text",
        Value::Bytes(_) => "bytea",
        Value::Temporal(TemporalValue::Interval { .. }) => "interval",
        Value::Temporal(_) => "timestamp",
        Value::Decimal(_) => "numeric",
        Value::List(_) => "anyarray",
        Value::Map(_) => "record",
    }
}

/// `function name(arg types) does not exist` - the error `PostgreSQL`
/// raises when call resolution fails (SQLSTATE 42883).
pub fn unknown_function_error(name: &str, args: &[(Option<String>, Value)]) -> SQLError {
    let types = args
        .iter()
        .map(|(arg_name, value)| match arg_name {
            Some(arg_name) => format!("{arg_name} => {}", value_type_name(value)),
            None => value_type_name(value).to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({types}) does not exist"),
    }
}

fn eval_binary(op: BinaryOp, lhs: &Expr, rhs: &Expr, ctx: &EvalContext<'_>) -> Result<Value> {
    if let Some(value) = eval_binary_borrowed(op, lhs, rhs, ctx)? {
        return Ok(value);
    }
    let l = eval(lhs, ctx)?;
    let r = eval(rhs, ctx)?;
    eval_binary_values(op, &l, &r)
}

/// Apply a binary SQL operator to values that have already been evaluated.
/// Execution engines use this when a hot path compiles expression traversal
/// ahead of time but must retain the evaluator's exact comparison, numeric
/// promotion, NULL, overflow, and division-by-zero semantics.
pub fn eval_binary_values(op: BinaryOp, l: &Value, r: &Value) -> Result<Value> {
    match op {
        BinaryOp::Equal
        | BinaryOp::NotEqual
        | BinaryOp::Less
        | BinaryOp::LessEqual
        | BinaryOp::Greater
        | BinaryOp::GreaterEqual => eval_comparison_op(op, l, r),
        BinaryOp::Add => arith(l, r, op),
        BinaryOp::Subtract => arith(l, r, op),
        BinaryOp::Multiply => arith(l, r, op),
        BinaryOp::Divide => arith(l, r, op),
    }
}

/// Comparison operators under SQL three-valued logic: any NULL operand
/// makes the result NULL.
fn eval_comparison_op(op: BinaryOp, l: &Value, r: &Value) -> Result<Value> {
    let out = match op {
        BinaryOp::Equal => values_equal_nullable(l, r).map(Value::Bool),
        BinaryOp::NotEqual => values_equal_nullable(l, r).map(|eq| Value::Bool(!eq)),
        BinaryOp::Less => compare_nullable(l, r)?.map(|ord| Value::Bool(ord.is_lt())),
        BinaryOp::LessEqual => compare_nullable(l, r)?.map(|ord| Value::Bool(ord.is_le())),
        BinaryOp::Greater => compare_nullable(l, r)?.map(|ord| Value::Bool(ord.is_gt())),
        BinaryOp::GreaterEqual => compare_nullable(l, r)?.map(|ord| Value::Bool(ord.is_ge())),
        _ => unreachable!("non-comparison op routed through eval_comparison_op"),
    };
    Ok(out.unwrap_or(Value::Null))
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
    Ok(Some(eval_comparison_op(op, l, r)?))
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
        Expr::Column(name) => Ok(Some(match ctx.row_lookup()?.column(name) {
            Some(value) => EvalOperand::Borrowed(value),
            None => EvalOperand::Owned(Value::Null),
        })),
        Expr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => Ok(Some(
            match ctx.row_lookup()?.qualified_column(qualifier, column, key) {
                Some(value) => EvalOperand::Borrowed(value),
                None => EvalOperand::Owned(Value::Null),
            },
        )),
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

/// Two-valued equality used where SQL treats a NULL comparison as
/// simply "no match" (CASE base matching, NULLIF, IN-subquery probes).
fn values_equal(a: &Value, b: &Value) -> bool {
    values_equal_nullable(a, b) == Some(true)
}

/// Three-valued equality: `None` when either side is NULL (or, for row
/// values, when element NULLs leave the outcome undecided).
fn values_equal_nullable(a: &Value, b: &Value) -> Option<bool> {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => None,
        (Value::Int(x), Value::Float(y)) => Some((*x as f64) == *y),
        (Value::Float(x), Value::Int(y)) => Some(*x == (*y as f64)),
        (Value::Decimal(x), Value::Decimal(y)) => Some(x == y),
        (Value::Int(x), Value::Decimal(y)) | (Value::Decimal(y), Value::Int(x)) => {
            Some(DecimalValue::from_i64(*x) == *y)
        }
        (Value::Float(x), Value::Decimal(y)) | (Value::Decimal(y), Value::Float(x)) => {
            Some(DecimalValue::from_f64_lossy(*x).is_some_and(|x| x == *y))
        }
        (Value::Bool(x), Value::Decimal(y)) | (Value::Decimal(y), Value::Bool(x)) => {
            Some(DecimalValue::from_bool(*x) == *y)
        }
        // Temporal equality goes through the ordering key so
        // `interval '1 mon' = interval '30 days'` holds like in
        // PostgreSQL (30-day months for comparison purposes).
        (Value::Temporal(x), Value::Temporal(y)) => Some(x.cmp(y) == std::cmp::Ordering::Equal),
        (Value::Temporal(x), Value::Str(y)) | (Value::Str(y), Value::Temporal(x)) => Some(
            x.parse_same_kind(y)
                .is_some_and(|parsed| x.cmp(&parsed) == std::cmp::Ordering::Equal),
        ),
        // Row / array equality: any definite mismatch wins, otherwise a
        // NULL element makes the whole comparison unknown (PostgreSQL
        // row comparison semantics).
        (Value::List(xs), Value::List(ys)) => {
            if xs.len() != ys.len() {
                return Some(false);
            }
            let mut unknown = false;
            for (x, y) in xs.iter().zip(ys) {
                match values_equal_nullable(x, y) {
                    Some(false) => return Some(false),
                    Some(true) => {}
                    None => unknown = true,
                }
            }
            if unknown {
                None
            } else {
                Some(true)
            }
        }
        _ => Some(a == b),
    }
}

fn compare(a: &Value, b: &Value) -> Result<std::cmp::Ordering> {
    Ok(compare_nullable(a, b)?.unwrap_or(std::cmp::Ordering::Equal))
}

/// Three-valued ordering: `None` when a NULL operand (or an undecided
/// NULL row element) leaves the comparison unknown.
fn compare_nullable(a: &Value, b: &Value) -> Result<Option<std::cmp::Ordering>> {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => Ok(None),
        (Value::Int(x), Value::Int(y)) => Ok(Some(x.cmp(y))),
        (Value::Float(x), Value::Float(y)) => Ok(Some(x.partial_cmp(y).unwrap_or(Ordering::Equal))),
        (Value::Int(x), Value::Float(y)) => {
            Ok(Some((*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal)))
        }
        (Value::Float(x), Value::Int(y)) => {
            Ok(Some(x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal)))
        }
        (Value::Decimal(x), Value::Decimal(y)) => Ok(Some(x.cmp(y))),
        (Value::Int(x), Value::Decimal(y)) => Ok(Some(DecimalValue::from_i64(*x).cmp(y))),
        (Value::Decimal(x), Value::Int(y)) => Ok(Some(x.cmp(&DecimalValue::from_i64(*y)))),
        (Value::Float(x), Value::Decimal(y)) => DecimalValue::from_f64_lossy(*x)
            .map(|x| Some(x.cmp(y)))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Decimal(x), Value::Float(y)) => DecimalValue::from_f64_lossy(*y)
            .map(|y| Some(x.cmp(&y)))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Bool(x), Value::Decimal(y)) => Ok(Some(DecimalValue::from_bool(*x).cmp(y))),
        (Value::Decimal(x), Value::Bool(y)) => Ok(Some(x.cmp(&DecimalValue::from_bool(*y)))),
        (Value::Str(x), Value::Str(y)) => Ok(Some(x.cmp(y))),
        (Value::Temporal(x), Value::Temporal(y)) => Ok(Some(x.cmp(y))),
        (Value::Temporal(x), Value::Str(y)) => x
            .parse_same_kind(y)
            .map(|parsed| Some(x.cmp(&parsed)))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Str(x), Value::Temporal(y)) => y
            .parse_same_kind(x)
            .map(|parsed| Some(parsed.cmp(y)))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Bool(x), Value::Bool(y)) => Ok(Some(x.cmp(y))),
        // Row / array ordering: lexicographic, with NULL elements
        // making the comparison unknown once reached before a decision.
        (Value::List(xs), Value::List(ys)) => {
            for (x, y) in xs.iter().zip(ys) {
                match compare_nullable(x, y)? {
                    Some(Ordering::Equal) => {}
                    Some(other) => return Ok(Some(other)),
                    None => return Ok(None),
                }
            }
            Ok(Some(xs.len().cmp(&ys.len())))
        }
        (lhs, rhs) => Err(SQLError::TypeMismatch(format!(
            "cannot compare {lhs:?} with {rhs:?}"
        ))),
    }
}

/// `PostgreSQL` `division by zero` error (SQLSTATE 22012).
pub(crate) fn division_by_zero() -> SQLError {
    SQLError::Routine {
        sqlstate: "22012".into(),
        message: "division by zero".into(),
    }
}

/// `PostgreSQL` numeric overflow error (SQLSTATE 22003).
pub(crate) fn out_of_range(type_name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22003".into(),
        message: format!("{type_name} out of range"),
    }
}

fn arith(a: &Value, b: &Value, op: BinaryOp) -> Result<Value> {
    // SQL three-valued logic: NULL `op` anything == NULL.
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return Ok(Value::Null);
    }
    // Integer x integer is the overwhelmingly common analytical path.
    // Resolve it before probing unrelated temporal / decimal / floating
    // representations, while retaining PostgreSQL overflow behavior. Integer
    // literals are represented as i64 here, so small-literal int4 overflow
    // remains the evaluator's existing deliberate PostgreSQL divergence.
    if let (Value::Int(li), Value::Int(ri)) = (a, b) {
        let out = match op {
            BinaryOp::Add => li.checked_add(*ri),
            BinaryOp::Subtract => li.checked_sub(*ri),
            BinaryOp::Multiply => li.checked_mul(*ri),
            BinaryOp::Divide => {
                if *ri == 0 {
                    return Err(division_by_zero());
                }
                // Integer / integer in SQL truncates toward zero.
                li.checked_div(*ri)
            }
            _ => unreachable!("non-arith op routed through arith"),
        };
        return out.map(Value::Int).ok_or_else(|| out_of_range("bigint"));
    }
    if matches!(op, BinaryOp::Subtract) && matches!(a, Value::Map(_) | Value::List(_)) {
        if let Some(value) = json_delete(&[a.clone(), b.clone()])? {
            return Ok(value);
        }
    }
    if matches!(a, Value::Temporal(_)) || matches!(b, Value::Temporal(_)) {
        return time::temporal_arith(a, b, op);
    }
    let has_decimal = matches!(a, Value::Decimal(_)) || matches!(b, Value::Decimal(_));
    let has_float = matches!(a, Value::Float(_)) || matches!(b, Value::Float(_));
    // PostgreSQL numeric promotion: double precision wins mixed
    // float/numeric arithmetic. Exact decimal arithmetic only applies
    // when no float operand is involved.
    if has_decimal && !has_float {
        return decimal_arith(a, b, op);
    }
    let lf = to_f64(a)?;
    let rf = to_f64(b)?;
    let result = match op {
        BinaryOp::Add => lf + rf,
        BinaryOp::Subtract => lf - rf,
        BinaryOp::Multiply => lf * rf,
        BinaryOp::Divide => {
            if rf == 0.0 {
                return Err(division_by_zero());
            }
            lf / rf
        }
        _ => unreachable!("non-arith op routed through arith"),
    };
    Ok(Value::Float(result))
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
                return Err(division_by_zero());
            }
            left.checked_div(&right)
        }
        _ => unreachable!("non-arith op routed through decimal_arith"),
    }
    .ok_or_else(|| out_of_range("numeric"))?;
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
                start.saturating_add(len)
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
            if args.iter().any(|arg| matches!(arg, Value::Null)) {
                return Ok(Value::Null);
            }
            match (&args[0], &args[1]) {
                (Value::Int(_), Value::Int(0)) => Err(division_by_zero()),
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
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
                        Ok(Value::List(vec![Value::Str(
                            caps.get(0).unwrap().as_str().into(),
                        )]))
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
                let result = v.log(base);
                // log(numeric, numeric) is numeric in PostgreSQL and
                // renders with 16-17 significant digits.
                let float_input = args.iter().any(|arg| matches!(arg, Value::Float(_)));
                if float_input {
                    Ok(Value::Float(result))
                } else {
                    Ok(DecimalValue::parse(&format!("{result:.16}"))
                        .map_or(Value::Float(result), Value::Decimal))
                }
            }
            _ => Err(SQLError::TypeMismatch("log takes 1 or 2 args".into())),
        },
        "log2" => float1(args, "log2", f64::log2),
        // Route cbrt through exp(ln(x)/3): this reproduces glibc's
        // last-ulp behavior (`cbrt(27)` = 3.0000000000000004), which is
        // what PostgreSQL emits on Linux builds; platform `cbrt` on
        // macOS is correctly rounded and would diverge.
        "cbrt" => float1(args, "cbrt", |x| {
            if x == 0.0 {
                0.0
            } else {
                x.signum() * (x.abs().ln() / 3.0).exp()
            }
        }),
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
            let owned;
            let bytes: &[u8] = match &args[0] {
                Value::Bytes(b) => b,
                other => {
                    owned = value_to_string(other).into_bytes();
                    &owned
                }
            };
            let encoding = value_to_string(&args[1]);
            match encoding.as_str() {
                "hex" => Ok(Value::Str(hex_encode(bytes))),
                "escape" => Ok(Value::Str(
                    String::from_utf8_lossy(bytes).escape_default().collect(),
                )),
                "base64" => Ok(Value::Str(base64_encode(bytes))),
                other => Err(SQLError::TypeMismatch(format!(
                    "unknown encoding {other:?}"
                ))),
            }
        }
        "decode" => {
            // decode() produces bytea; the result renders as
            // PostgreSQL hex output (`\x616263`) at the SQL boundary.
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("decode takes 2 args".into()));
            }
            let s = value_to_string(&args[0]);
            let encoding = value_to_string(&args[1]);
            match encoding.as_str() {
                "hex" => {
                    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
                    if cleaned.len() % 2 != 0 {
                        return Err(SQLError::TypeMismatch(
                            "invalid hexadecimal data: odd number of digits".into(),
                        ));
                    }
                    let mut out = Vec::with_capacity(cleaned.len() / 2);
                    let bytes = cleaned.as_bytes();
                    let mut i = 0;
                    while i + 1 < bytes.len() {
                        let hi = (bytes[i] as char).to_digit(16).ok_or_else(|| {
                            SQLError::TypeMismatch("invalid hexadecimal digit".into())
                        })? as u8;
                        let lo = (bytes[i + 1] as char).to_digit(16).ok_or_else(|| {
                            SQLError::TypeMismatch("invalid hexadecimal digit".into())
                        })? as u8;
                        out.push(hi * 16 + lo);
                        i += 2;
                    }
                    Ok(Value::Bytes(out))
                }
                "base64" => base64_decode(&s)
                    .map(Value::Bytes)
                    .map_err(|e| SQLError::TypeMismatch(format!("base64 decode: {e}"))),
                "escape" => Ok(Value::Bytes(s.into_bytes())),
                other => Err(SQLError::TypeMismatch(format!(
                    "unknown encoding {other:?}"
                ))),
            }
        }
        "split_part" => {
            // Negative positions count from the end; zero errors
            // (PostgreSQL `field position must not be zero`).
            if args.len() != 3 {
                return Err(SQLError::TypeMismatch("split_part takes 3 args".into()));
            }
            if args.iter().any(|arg| matches!(arg, Value::Null)) {
                return Ok(Value::Null);
            }
            let s = value_to_string(&args[0]);
            let sep = value_to_string(&args[1]);
            let idx = to_i64(&args[2])?;
            if idx == 0 {
                return Err(SQLError::Routine {
                    sqlstate: "22023".into(),
                    message: "field position must not be zero".into(),
                });
            }
            let parts: Vec<&str> = if sep.is_empty() {
                vec![s.as_str()]
            } else {
                s.split(sep.as_str()).collect()
            };
            let idx_usize = if idx >= 1 {
                (idx - 1) as usize
            } else {
                let from_end = idx.unsigned_abs() as usize;
                if from_end > parts.len() {
                    return Ok(Value::Str(String::new()));
                }
                parts.len() - from_end
            };
            Ok(Value::Str(
                parts.get(idx_usize).copied().unwrap_or("").to_string(),
            ))
        }
        "now" | "current_timestamp" => Ok(Value::Temporal(TemporalValue::TimestampTz {
            micros: chrono::Utc::now().timestamp_micros(),
        })),
        "current_date" => {
            let micros = chrono::Utc::now().timestamp_micros();
            Ok(Value::Temporal(TemporalValue::Date {
                days: (micros.div_euclid(86_400_000_000)) as i32,
            }))
        }
        "to_timestamp" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("to_timestamp takes 1 arg".into()));
            }
            let secs = to_f64(&args[0])?;
            Ok(Value::Temporal(TemporalValue::TimestampTz {
                micros: (secs * 1e6).round() as i64,
            }))
        }
        "extract" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch(
                    "extract takes 2 args (field, ts)".into(),
                ));
            }
            let field = value_to_string(&args[0]).to_ascii_lowercase();
            extract_from_value(&field, &args[1], true)
        }
        "date_part" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch(
                    "date_part takes 2 args (field, ts)".into(),
                ));
            }
            let field = value_to_string(&args[0]).to_ascii_lowercase();
            extract_from_value(&field, &args[1], false)
        }
        "age" => {
            let (a, b) = match args.len() {
                // One-argument age() measures against today's midnight.
                1 => {
                    let micros = chrono::Utc::now().timestamp_micros();
                    let midnight = micros.div_euclid(86_400_000_000) * 86_400_000_000;
                    (
                        coerce_temporal(&args[0])?,
                        TemporalValue::Timestamp { micros: midnight },
                    )
                }
                2 => (coerce_temporal(&args[0])?, coerce_temporal(&args[1])?),
                _ => return Err(SQLError::TypeMismatch("age takes 1-2 args".into())),
            };
            age_between(&a, &b)
        }
        "date_trunc" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("date_trunc takes 2 args".into()));
            }
            let unit = value_to_string(&args[0]).to_ascii_lowercase();
            date_trunc_value(&unit, &args[1])
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
            let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
            chrono::NaiveDate::from_ymd_opt(year, month, day)
                .map(|d| {
                    Value::Temporal(TemporalValue::Date {
                        days: d.signed_duration_since(epoch).num_days() as i32,
                    })
                })
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!(
                        "make_date: invalid date {year:04}-{month:02}-{day:02}"
                    ))
                })
        }
        "make_interval" => {
            // make_interval(years, months, weeks, days, hours, mins,
            // secs) -> PostgreSQL's months/days/micros interval model.
            let years = args.first().map(to_i64).transpose()?.unwrap_or(0);
            let months = args.get(1).map(to_i64).transpose()?.unwrap_or(0);
            let weeks = args.get(2).map(to_i64).transpose()?.unwrap_or(0);
            let days = args.get(3).map(to_i64).transpose()?.unwrap_or(0);
            let hours = args.get(4).map(to_i64).transpose()?.unwrap_or(0);
            let mins = args.get(5).map(to_i64).transpose()?.unwrap_or(0);
            let secs = args.get(6).map(to_f64).transpose()?.unwrap_or(0.0);
            let total_months =
                i32::try_from(years * 12 + months).map_err(|_| out_of_range("interval"))?;
            let total_days =
                i32::try_from(weeks * 7 + days).map_err(|_| out_of_range("interval"))?;
            let micros = (hours * 3_600 + mins * 60) * 1_000_000 + (secs * 1e6).round() as i64;
            Ok(Value::Temporal(TemporalValue::Interval {
                months: total_months,
                days: total_days,
                micros,
            }))
        }
        "justify_hours" => {
            if let Some(Value::Temporal(TemporalValue::Interval {
                months,
                days,
                micros,
            })) = args.first()
            {
                let extra_days = micros.div_euclid(86_400_000_000);
                return Ok(Value::Temporal(TemporalValue::Interval {
                    months: *months,
                    days: days + extra_days as i32,
                    micros: micros.rem_euclid(86_400_000_000),
                }));
            }
            Err(SQLError::TypeMismatch(
                "justify_hours takes an interval".into(),
            ))
        }
        "to_char" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("to_char takes 2 args".into()));
            }
            if args.iter().any(|arg| matches!(arg, Value::Null)) {
                return Ok(Value::Null);
            }
            let fmt = value_to_string(&args[1]);
            match &args[0] {
                Value::Int(i) => Ok(Value::Str(format_pg_number(*i as f64, &fmt))),
                Value::Float(f) => Ok(Value::Str(format_pg_number(*f, &fmt))),
                Value::Decimal(d) => d
                    .to_f64()
                    .map(|value| Value::Str(format_pg_number(value, &fmt)))
                    .ok_or_else(|| SQLError::TypeMismatch("to_char: numeric out of range".into())),
                Value::Temporal(t) => Ok(Value::Str(format_temporal(t, &fmt)?)),
                Value::Str(s) => {
                    let temporal = coerce_temporal(&Value::Str(s.clone()))?;
                    Ok(Value::Str(format_temporal(&temporal, &fmt)?))
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
            let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
            Ok(Value::Temporal(TemporalValue::Date {
                days: date.signed_duration_since(epoch).num_days() as i32,
            }))
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
                // The temporal model has no infinity values, so every
                // date / timestamp / interval is finite.
                Value::Int(_) | Value::Decimal(_) | Value::Str(_) | Value::Temporal(_) => {
                    Ok(Value::Bool(true))
                }
                Value::Null => Ok(Value::Null),
                other => Err(SQLError::TypeMismatch(format!(
                    "isfinite: unsupported {other:?}"
                ))),
            }
        }
        "clock_timestamp" | "statement_timestamp" => {
            Ok(Value::Temporal(TemporalValue::TimestampTz {
                micros: chrono::Utc::now().timestamp_micros(),
            }))
        }
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
            // Casts materialize jsonb scalars as engine values, so type
            // the value directly; only bare strings re-parse (and an
            // unparsable string IS a JSON string).
            let type_name = match &args[0] {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Int(_) | Value::Float(_) | Value::Decimal(_) => "number",
                Value::List(_) => "array",
                Value::Map(_) => "object",
                Value::Str(s) => match parse_json(s) {
                    Ok(parsed) => json_typeof(&parsed),
                    Err(_) => "string",
                },
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "json_typeof: unsupported {other:?}"
                    )));
                }
            };
            Ok(Value::Str(type_name.to_string()))
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
        // Documented divergence: `to_jsonb('text')` produces the JSON
        // string as a plain engine string, which renders unquoted at
        // the SQL boundary (PostgreSQL shows `"text"`). The Value model
        // has no jsonb-scalar tag to preserve the distinction.
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
                // Empty arrays have no dimensions in PostgreSQL, so
                // `array_length('{}', 1)` is NULL (not 0). Dimensions
                // other than 1 are NULL for the 1-D arrays the engine
                // stores.
                Value::List(items) => {
                    let dim = args.get(1).map(to_i64).transpose()?.unwrap_or(1);
                    if items.is_empty() || dim != 1 {
                        return Ok(Value::Null);
                    }
                    Ok(Value::Int(items.len() as i64))
                }
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
            let start = args.get(2).map(to_i64).transpose()?.unwrap_or(1).max(1) as usize;
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
        // setseed() reseeds PostgreSQL's per-session random() state.
        // The engine's random() is time-derived and non-seedable, so
        // this accepts the call for compatibility and returns void
        // (rendered as an empty string, like psql shows void);
        // random() reproducibility is a documented divergence.
        "setseed" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("setseed takes 1 arg".into()));
            }
            let seed = to_f64(&args[0])?;
            if !(-1.0..=1.0).contains(&seed) {
                return Err(SQLError::Routine {
                    sqlstate: "22023".into(),
                    message: format!("setseed parameter {seed} is out of allowed range [-1,1]"),
                });
            }
            Ok(Value::Str(String::new()))
        }
        "num_nulls" => Ok(Value::Int(
            args.iter().filter(|v| matches!(v, Value::Null)).count() as i64,
        )),
        "num_nonnulls" => Ok(Value::Int(
            args.iter().filter(|v| !matches!(v, Value::Null)).count() as i64,
        )),
        // The engine has a single database and a single flat namespace;
        // these identifiers exist for PostgreSQL client compatibility.
        "current_database" | "current_catalog" => Ok(Value::Str("uqa".into())),
        "current_user" | "session_user" => Ok(Value::Str("uqa".into())),
        "current_schema" => Ok(Value::Str("public".into())),
        "current_schemas" => Ok(Value::List(vec![
            Value::Str("pg_catalog".into()),
            Value::Str("public".into()),
        ])),
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
            let n = to_i64(&dims[0])?.max(0) as usize;
            Ok(Value::List(vec![args[0].clone(); n]))
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
            if n < 0 || n as usize > items.len() {
                return Err(SQLError::Routine {
                    sqlstate: "2202E".into(),
                    message: format!(
                        "number of elements to trim must be between 0 and {}",
                        items.len()
                    ),
                });
            }
            Ok(Value::List(items[..items.len() - n as usize].to_vec()))
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
            if n < 0 || n as usize > items.len() {
                return Err(SQLError::Routine {
                    sqlstate: "22023".into(),
                    message: format!("sample size must be between 0 and {}", items.len()),
                });
            }
            let mut pool = items.clone();
            let mut out = Vec::with_capacity(n as usize);
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
                (Value::List(a), Value::List(b)) => {
                    Ok(Value::Bool(a.iter().any(|x| {
                        !matches!(x, Value::Null) && b.iter().any(|y| values_equal(x, y))
                    })))
                }
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
                    if idx < 1 || idx as usize > items.len() {
                        return Ok(Value::Null);
                    }
                    Ok(items[(idx - 1) as usize].clone())
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
                    .max(1) as usize;
                    let hi = match &args[2] {
                        Value::Null => items.len() as i64,
                        other => to_i64(other)?,
                    }
                    .min(items.len() as i64);
                    if hi < lo as i64 {
                        return Ok(Value::List(Vec::new()));
                    }
                    Ok(Value::List(items[lo - 1..hi as usize].to_vec()))
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
        other => Err(SQLError::UnknownFunction(other.to_string())),
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
            TemporalValue::Interval { .. } => "interval".into(),
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

/// `trim` / `ltrim` / `rtrim` / `btrim` with the optional
/// character-SET second argument (defaults to whitespace).
fn trim_chars(args: &[Value], start: bool, end: bool) -> Result<Value> {
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
fn compile_pg_regex(pattern: &str, flags: &str) -> Result<regex::Regex> {
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
fn is_quoted_keyword(word: &str) -> bool {
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
fn quote_ident(ident: &str) -> String {
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
fn quote_literal(text: &str) -> String {
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
fn similar_to_regex(pattern: &str) -> String {
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

/// Cast a value to the named SQL type, mirroring `CAST(expr AS ty)`.
/// Types outside the engine's coercion surface return
/// [`SQLError::Unsupported`]; callers doing best-effort typing (the
/// `PL/pgSQL` interpreter) treat that as "leave the value as-is".
pub fn cast_value(v: &Value, ty: &str) -> Result<Value> {
    if matches!(v, Value::Null) {
        return Ok(Value::Null);
    }
    if let Some(elem_ty) = ty.strip_suffix("[]") {
        // `'{1,2,3}'::int[]` parses the PostgreSQL array literal
        // before casting each element.
        let items: Vec<Value> = match v {
            Value::List(items) => items.clone(),
            Value::Str(s) => parse_pg_array_literal(s)?,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "CAST AS {ty}: expected array, got {other:?}"
                )));
            }
        };
        return items
            .iter()
            .map(|item| cast_value(item, elem_ty))
            .collect::<Result<Vec<_>>>()
            .map(Value::List);
    }
    let (base, modifier) = split_type_modifier(ty);
    match base {
        "smallint" | "int2" | "pg_catalog.int2" => cast_integer(v, "smallint"),
        "integer" | "int" | "int4" | "serial" | "serial4" | "pg_catalog.int4" => {
            cast_integer(v, "integer")
        }
        "bigint" | "int8" | "bigserial" | "serial8" | "pg_catalog.int8" => {
            cast_integer(v, "bigint")
        }
        "real" | "float4" | "float8" | "double" | "double precision" => {
            Ok(Value::Float(to_f64(v)?))
        }
        "numeric" | "decimal" => {
            let value = to_decimal(v)?;
            if let Some(modifier) = modifier {
                let mut parts = modifier.split(',').map(str::trim);
                let precision: u32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| SQLError::TypeMismatch("bad numeric precision".into()))?;
                let scale: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let rounded = value
                    .round_to_scale(scale)
                    .ok_or_else(|| out_of_range("numeric"))?;
                if !rounded.fits_precision(precision, scale) {
                    return Err(SQLError::Routine {
                        sqlstate: "22003".into(),
                        message: format!(
                            "numeric field overflow: A field with precision {precision}, scale {scale} cannot hold value {}",
                            value.to_sql_string()
                        ),
                    });
                }
                return Ok(Value::Decimal(rounded));
            }
            Ok(Value::Decimal(value))
        }
        "text" | "name" | "uuid" => Ok(Value::Str(value_to_string(v))),
        // char(n) / varchar(n): the explicit cast TRUNCATES to the
        // declared length (PostgreSQL `'abc'::char(2)` = 'ab').
        "varchar" | "character varying" | "character" | "char" | "bpchar" => {
            let text = value_to_string(v);
            let Some(modifier) = modifier else {
                return Ok(Value::Str(text));
            };
            let limit: usize = modifier
                .trim()
                .parse()
                .map_err(|_| SQLError::TypeMismatch(format!("bad length modifier {modifier}")))?;
            Ok(Value::Str(text.chars().take(limit).collect()))
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
        "interval" => cast_temporal(v, TemporalValue::parse_interval, "interval"),
        // Documented divergences from PostgreSQL: (1) `json` (non-b)
        // does not preserve the source text - objects land in the same
        // key-sorted Map representation as `jsonb`; (2) top-level jsonb
        // scalars materialize as plain engine values, so a jsonb string
        // renders without JSON quotes.
        "json" | "jsonb" => Ok(json_to_value(&parse_json(&value_to_string(v))?)),
        "bytea" => match v {
            Value::Bytes(bytes) => Ok(Value::Bytes(bytes.clone())),
            // PostgreSQL reads `\x...` hex input for bytea.
            Value::Str(s) if s.starts_with("\\x") => {
                let hex = &s[2..];
                let mut out = Vec::with_capacity(hex.len() / 2);
                let bytes = hex.as_bytes();
                let mut i = 0;
                while i + 1 < bytes.len() {
                    let hi = (bytes[i] as char)
                        .to_digit(16)
                        .ok_or_else(|| SQLError::TypeMismatch("invalid hex in bytea".into()))?;
                    let lo = (bytes[i + 1] as char)
                        .to_digit(16)
                        .ok_or_else(|| SQLError::TypeMismatch("invalid hex in bytea".into()))?;
                    out.push((hi * 16 + lo) as u8);
                    i += 2;
                }
                Ok(Value::Bytes(out))
            }
            Value::Str(s) => Ok(Value::Bytes(s.as_bytes().to_vec())),
            other => Ok(Value::Bytes(value_to_string(other).into_bytes())),
        },
        "boolean" | "bool" => cast_boolean(v),
        other => Err(SQLError::Unsupported(format!("CAST AS {other}"))),
    }
}

/// Split `varchar(10)` / `numeric(10,2)` into `("varchar", Some("10"))`.
fn split_type_modifier(ty: &str) -> (&str, Option<&str>) {
    match (ty.find('('), ty.rfind(')')) {
        (Some(open), Some(close)) if close > open => {
            (ty[..open].trim_end(), Some(&ty[open + 1..close]))
        }
        _ => (ty, None),
    }
}

/// CAST to the integer family with `PostgreSQL` conversion rules:
/// float8 rounds half-to-even, numeric rounds half-away-from-zero,
/// strings must be integral text, and the result must fit the target
/// width.
fn cast_integer(v: &Value, target: &str) -> Result<Value> {
    let n: i64 = match v {
        Value::Int(n) => *n,
        Value::Bool(b) => i64::from(*b),
        Value::Float(f) => {
            if !f.is_finite() {
                return Err(out_of_range(target));
            }
            let rounded = f.round_ties_even();
            if rounded < i64::MIN as f64 || rounded > i64::MAX as f64 {
                return Err(out_of_range(target));
            }
            rounded as i64
        }
        Value::Decimal(d) => d
            .round_dp(0)
            .to_i64_trunc()
            .ok_or_else(|| out_of_range(target))?,
        Value::Str(s) => s.trim().parse::<i64>().map_err(|_| SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("invalid input syntax for type {target}: \"{s}\""),
        })?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to {target}"
            )));
        }
    };
    let in_range = match target {
        "smallint" => i16::try_from(n).is_ok(),
        "integer" => i32::try_from(n).is_ok(),
        _ => true,
    };
    if !in_range {
        return Err(out_of_range(target));
    }
    Ok(Value::Int(n))
}

/// CAST to boolean: strings follow `PostgreSQL`'s `parse_bool`
/// (prefixes of true/false/yes/no, on/off, 1/0); numbers are non-zero
/// tests.
fn cast_boolean(v: &Value) -> Result<Value> {
    match v {
        Value::Bool(b) => Ok(Value::Bool(*b)),
        Value::Int(n) => Ok(Value::Bool(*n != 0)),
        Value::Float(f) => Ok(Value::Bool(*f != 0.0)),
        Value::Decimal(d) => Ok(Value::Bool(!d.is_zero())),
        Value::Str(s) => {
            let text = s.trim().to_ascii_lowercase();
            let matches_prefix = |word: &str| !text.is_empty() && word.starts_with(&text);
            let value = if matches_prefix("true") || matches_prefix("yes") || text == "1" {
                Some(true)
            } else if matches_prefix("false") || matches_prefix("no") || text == "0" {
                Some(false)
            } else if "on" == text {
                Some(true)
            } else if matches_prefix("off") && text.len() >= 2 {
                Some(false)
            } else {
                None
            };
            value.map(Value::Bool).ok_or_else(|| SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type boolean: \"{s}\""),
            })
        }
        other => Err(SQLError::TypeMismatch(format!(
            "cannot cast {other:?} to boolean"
        ))),
    }
}

/// Parse a `PostgreSQL` array literal (`{1,2,3}`, `{"a b",NULL}`)
/// into a list of string/NULL values; the caller casts elements.
fn parse_pg_array_literal(text: &str) -> Result<Vec<Value>> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("malformed array literal: \"{text}\""),
        })?;
    let mut items: Vec<Value> = Vec::new();
    let mut current = String::new();
    let mut chars = inner.chars().peekable();
    let mut in_quotes = false;
    let mut was_quoted = false;
    let push_item = |raw: &str, quoted: bool, items: &mut Vec<Value>| {
        let value = raw.trim();
        if value.is_empty() && !quoted {
            return;
        }
        if !quoted && value.eq_ignore_ascii_case("null") {
            items.push(Value::Null);
        } else {
            items.push(Value::Str(value.to_string()));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                was_quoted = true;
            }
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ',' if !in_quotes => {
                push_item(&current, was_quoted, &mut items);
                current.clear();
                was_quoted = false;
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() || was_quoted {
        push_item(&current, was_quoted, &mut items);
    }
    Ok(items)
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
        // bytea renders as PostgreSQL hex output in text contexts.
        Value::Bytes(b) => format!("\\x{}", hex_encode(b)),
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
            .trim()
            .parse()
            .map_err(|_| SQLError::TypeMismatch(format!("cannot parse {s:?} as integer"))),
        other => Err(SQLError::TypeMismatch(format!(
            "expected integer, got {other:?}"
        ))),
    }
}

pub(crate) fn to_f64(v: &Value) -> Result<f64> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Decimal(d) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch(format!("cannot cast {v:?} to double precision"))
        }),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        // float8 casts accept PostgreSQL's textual forms, including
        // Infinity / NaN spellings.
        Value::Str(s) => {
            let text = s.trim();
            let lowered = text.to_ascii_lowercase();
            match lowered.as_str() {
                "infinity" | "inf" | "+infinity" | "+inf" => Ok(f64::INFINITY),
                "-infinity" | "-inf" => Ok(f64::NEG_INFINITY),
                "nan" => Ok(f64::NAN),
                _ => text.parse().map_err(|_| SQLError::Routine {
                    sqlstate: "22P02".into(),
                    message: format!("invalid input syntax for type double precision: \"{s}\""),
                }),
            }
        }
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
    fn projected_row_lookup_evaluates_columns_without_a_result_map() {
        struct ProjectedRow {
            names: [&'static str; 2],
            values: [Value; 2],
        }

        impl RowLookup for ProjectedRow {
            fn column(&self, name: &str) -> Option<&Value> {
                self.names
                    .iter()
                    .position(|candidate| *candidate == name)
                    .and_then(|index| self.values.get(index))
            }

            fn qualified_column(
                &self,
                _qualifier: &str,
                column: &str,
                _key: &str,
            ) -> Option<&Value> {
                self.column(column)
            }
        }

        let row = ProjectedRow {
            names: ["quantity", "status"],
            values: [Value::Int(7), Value::Str("O".into())],
        };
        let ctx = EvalContext::from_row_lookup(&row, &[]);

        assert_eq!(
            eval(&Expr::Column("quantity".into()), &ctx).unwrap(),
            Value::Int(7)
        );
        assert_eq!(
            eval(
                &Expr::QualifiedColumn {
                    qualifier: "lineitem".into(),
                    column: "status".into(),
                    key: String::new(),
                },
                &ctx,
            )
            .unwrap(),
            Value::Str("O".into())
        );
        assert_eq!(
            eval(
                &Expr::Binary {
                    op: BinaryOp::Greater,
                    lhs: Box::new(Expr::Column("quantity".into())),
                    rhs: Box::new(Expr::Literal(Value::Int(5))),
                },
                &ctx,
            )
            .unwrap(),
            Value::Bool(true)
        );
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
