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
mod binary;
mod casting;
mod conversion;
mod scalar_array;
mod scalar_core;
mod scalar_dispatch;
mod scalar_geospatial;
mod scalar_helpers;
mod scalar_json;
mod scalar_math;
mod scalar_postgres;
mod scalar_temporal;

use binary::{
    compare, compare_nullable, eval_binary, eval_comparison_op, row_column_value, values_equal,
    values_equal_nullable,
};
pub(crate) use binary::{division_by_zero, out_of_range};
pub use binary::{eval_binary_values, truthy};
pub use casting::{array_dimensions, cast_value, parse_pg_array_literal};
pub(crate) use conversion::to_f64;
use conversion::{
    allocation_error, coerce_i64, expect_str, float1, float_to_i64_rounded, float_to_i64_trunc,
    gcd_i64, initcap_str, nonnegative_usize, string1, to_decimal, to_i64, value_to_string,
};
pub use conversion::{value_to_tensor, value_to_vector};
use scalar_dispatch::{eval_scalar_function, eval_sequence_function};
use scalar_helpers::{
    compile_pg_regex, like_match, point_xy, quote_ident, quote_literal, similar_to_regex,
    trim_chars, typeof_value,
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

    /// Resolve the first existing schema on the logical session's search
    /// path. `None` lets standalone expression evaluation use its `public`
    /// compatibility default.
    fn current_schema(&self) -> std::result::Result<Option<String>, String> {
        Ok(None)
    }

    /// Resolve the existing schemas visible to the logical session.
    fn current_schemas(
        &self,
        _include_implicit: bool,
    ) -> std::result::Result<Option<Vec<String>>, String> {
        Ok(None)
    }

    /// Draw from an engine-owned logical-session PRNG. `None` keeps pure,
    /// engine-free expression evaluation available for library callers.
    fn random_value(&self) -> std::result::Result<Option<f64>, String> {
        Ok(None)
    }

    /// Reseed the logical-session PRNG. `false` means the hook does not own a
    /// mutable random stream and the caller must report the unsupported call.
    fn set_random_seed(&self, _seed: f64) -> std::result::Result<bool, String> {
        Ok(false)
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

    /// Return a value by the physical schema position used to construct this
    /// row view. Materialized named rows do not expose positional access;
    /// projected execution sources override it so compiled hot paths can avoid
    /// repeating string lookup for every expression and row.
    fn positional_column(&self, _index: usize) -> Option<&Value> {
        None
    }

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
        Expr::Param(i) => match i.checked_sub(1).and_then(|index| ctx.params.get(index)) {
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

    if lower == "random" {
        if !evaluated.is_empty() {
            return Err(SQLError::TypeMismatch("random takes no arguments".into()));
        }
        if let Some(engine) = ctx.engine {
            if let Some(value) = engine.random_value().map_err(SQLError::Internal)? {
                return Ok(Value::Float(value));
            }
        }
    }
    if lower == "setseed" {
        let [value] = evaluated.as_slice() else {
            return Err(SQLError::TypeMismatch("setseed takes 1 arg".into()));
        };
        let seed = to_f64(value)?;
        if !seed.is_finite() || !(-1.0..=1.0).contains(&seed) {
            return Err(SQLError::Routine {
                sqlstate: "22023".into(),
                message: format!("setseed parameter {seed} is out of allowed range [-1,1]"),
            });
        }
        let engine = ctx.engine.ok_or_else(|| {
            SQLError::Unsupported("setseed requires a logical engine session".into())
        })?;
        if !engine.set_random_seed(seed).map_err(SQLError::Internal)? {
            return Err(SQLError::Unsupported(
                "engine hook does not provide a session random stream".into(),
            ));
        }
        return Ok(Value::Str(String::new()));
    }

    if lower == "current_schema" {
        if !evaluated.is_empty() {
            return Err(SQLError::TypeMismatch(
                "current_schema takes no arguments".into(),
            ));
        }
        let schema = ctx
            .engine
            .map(|engine| engine.current_schema())
            .transpose()
            .map_err(SQLError::Internal)?
            .flatten()
            .unwrap_or_else(|| "public".to_string());
        return Ok(Value::Str(schema));
    }
    if lower == "current_schemas" {
        let [Value::Bool(include_implicit)] = evaluated.as_slice() else {
            return Err(SQLError::TypeMismatch(
                "current_schemas takes one boolean argument".into(),
            ));
        };
        let schemas = ctx
            .engine
            .map(|engine| engine.current_schemas(*include_implicit))
            .transpose()
            .map_err(SQLError::Internal)?
            .flatten()
            .unwrap_or_else(|| {
                let mut schemas = Vec::new();
                if *include_implicit {
                    schemas.push("pg_catalog".to_string());
                }
                schemas.push("public".to_string());
                schemas
            });
        return Ok(Value::List(schemas.into_iter().map(Value::Str).collect()));
    }

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

#[cfg(test)]
mod tests;
