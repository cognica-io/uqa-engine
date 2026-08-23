//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar expression evaluator: turns an [`Expr`] into a [`Value`] under
//! a row context (column -> value) and a parameter binding.

use std::borrow::Cow;

use uqa_core::{ArrayValue, DecimalValue, TemporalValue, Value};

use crate::ast::{BinaryOp, Expr};
use crate::error::{Result, SQLError};
use crate::params::SQLParam;
use crate::result::ResultRow;

mod encoding;
mod json;
mod time;
mod uuid;

use encoding::{base64_decode, base64_encode, md5_hex};
pub use json::value_to_json_text;
use json::{
    format_jsonb_pretty, json_build_array_value, json_build_object_value, json_concat,
    json_contained_by, json_contains, json_delete, json_delete_path, json_extract_path,
    json_has_key, json_has_keys, json_typeof, jsonb_insert, jsonb_set, jsonpath_candidate,
    jsonpath_exists, jsonpath_match, parse_json, strip_nulls, typed_json_value, value_to_json,
};
use time::{
    age_between, coerce_temporal, date_trunc_value, extract_from_value, format_pg_number,
    format_temporal, hex_encode, make_timestamp, parse_timestamp, pg_to_chrono_fmt,
};
use uuid::{extract_uuid_timestamp, extract_uuid_version, generate_random_uuid, generate_uuid_v7};
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
    compare, compare_nullable, eval_binary, eval_comparison_op, values_equal, values_equal_nullable,
};
pub(crate) use binary::{division_by_zero, out_of_range};
pub use binary::{
    eval_binary_values, eval_binary_values_with_integer_width, eval_comparison_truth,
    integer_width_for_literal, integer_width_for_type, truthy, IntegerWidth,
};
pub use casting::{
    array_dimensions, cast_value, cast_value_from, negate_value, parse_pg_array_literal,
};
pub(crate) use conversion::to_f64;
use conversion::{
    allocation_error, coerce_i64, expect_str, float1, float_to_i64_rounded, float_to_i64_trunc,
    gcd_i64, initcap_str, nonnegative_usize, string1, to_decimal, to_i64,
};
pub use conversion::{array_value_to_string, value_to_string};
pub use conversion::{value_to_tensor, value_to_vector};
use scalar_dispatch::{eval_scalar_function, eval_sequence_function};
use scalar_helpers::{
    compile_pg_regex, point_xy, quote_literal, similar_to_regex, trim_chars, typeof_value,
};
pub use scalar_helpers::{quote_ident, CompiledLikePattern};

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

    fn call_bound_user_function(
        &self,
        _binding: &crate::ast::FunctionBinding,
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

    /// Whether an unqualified name identifies more than one visible input
    /// column. Callers must report SQLSTATE 42702 instead of selecting an
    /// arbitrary suffix match.
    fn column_is_ambiguous(&self, _name: &str) -> bool {
        false
    }

    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value>;

    /// Whether a qualified identity names more than one visible input column.
    fn qualified_column_is_ambiguous(&self, _qualifier: &str, _column: &str) -> bool {
        false
    }

    /// Return a value by the physical schema position used to construct this
    /// row view. Materialized named rows do not expose positional access;
    /// projected execution sources override it so compiled hot paths can avoid
    /// repeating string lookup for every expression and row.
    fn positional_column(&self, _index: usize) -> Option<&Value> {
        None
    }

    /// Visit every logical column in schema order. Named rows use their map
    /// order; positional execution rows override this without materializing a
    /// map. The default keeps narrow projected lookup implementations source
    /// compatible when they deliberately do not expose whole-row semantics.
    fn visit_columns(&self, _visitor: &mut dyn FnMut(&str, &Value)) {}
}

impl RowLookup for ResultRow {
    fn column(&self, name: &str) -> Option<&Value> {
        self.get(name)
    }

    fn qualified_column(&self, _qualifier: &str, _column: &str) -> Option<&Value> {
        None
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        for (column, value) in self {
            visitor(column, value);
        }
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
            // Whole-row materialization is needed only by correlated
            // subqueries. Ordinary scalar evaluation must remain on the
            // lookup/slot path.
            row: None,
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
        if self.row_lookup()?.column_is_ambiguous(name) {
            return Err(SQLError::AmbiguousColumn(name.to_string()));
        }
        Ok(self
            .row_lookup()?
            .column(name)
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Resolve a qualified column without constructing an AST expression.
    pub fn qualified_column_value(&self, qualifier: &str, column: &str) -> Result<Value> {
        if self
            .row_lookup()?
            .qualified_column_is_ambiguous(qualifier, column)
        {
            return Err(SQLError::AmbiguousColumn(format!("{qualifier}.{column}")));
        }
        Ok(self
            .row_lookup()?
            .qualified_column(qualifier, column)
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
        Expr::Default => Err(SQLError::Internal(
            "DEFAULT reached scalar expression evaluation without a mutation target".into(),
        )),
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
            if ctx.row_lookup()?.column_is_ambiguous(name) {
                return Err(SQLError::AmbiguousColumn(name.clone()));
            }
            Ok(ctx
                .row_lookup()?
                .column(name)
                .cloned()
                .unwrap_or(Value::Null))
        }
        Expr::QualifiedColumn { qualifier, column } => {
            if ctx
                .row_lookup()?
                .qualified_column_is_ambiguous(qualifier, column)
            {
                return Err(SQLError::AmbiguousColumn(format!("{qualifier}.{column}")));
            }
            Ok(ctx
                .row_lookup()?
                .qualified_column(qualifier, column)
                .cloned()
                .unwrap_or(Value::Null))
        }
        Expr::Array(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for e in elements {
                out.push(eval(e, ctx)?);
            }
            ArrayValue::try_new(out).map(Value::Array).ok_or_else(|| {
                SQLError::TypeMismatch(
                    "multidimensional arrays must have matching dimensions".into(),
                )
            })
        }
        Expr::Row(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for element in elements {
                out.push(eval(element, ctx)?);
            }
            Ok(Value::Row(out))
        }
        Expr::Star | Expr::QualifiedStar(_) => {
            Err(SQLError::Internal("`*` cannot be evaluated".into()))
        }
        Expr::Func {
            name,
            binding,
            args,
            ..
        } => {
            let call_args = evaluate_call_args(args, ctx)?;
            if let Some(binding) = binding {
                let engine = ctx.engine.ok_or_else(|| {
                    SQLError::Unsupported(
                        "bound user function requires a logical engine session".into(),
                    )
                })?;
                engine
                    .call_bound_user_function(binding, &call_args)
                    .unwrap_or_else(|| Err(SQLError::UnknownFunction(binding.name.clone())))
            } else {
                eval_function_call(name, call_args, ctx)
            }
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
            let source_ty = explicit_expr_type(expr);
            let v = eval(expr, ctx)?;
            cast_value_from(&v, ty, source_ty)
        }
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
            Err(SQLError::Unsupported(
                "query-valued expressions must be lowered to physical ScalarExpr/QueryPlan slots"
                    .into(),
            ))
        }
        Expr::Binary { op, lhs, rhs } => eval_binary(*op, lhs, rhs, ctx),
        Expr::UnaryMinus(inner) => {
            let source_ty = explicit_expr_type(inner);
            let value = eval(inner, ctx)?;
            negate_value(&value, source_ty)
        }
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

fn explicit_expr_type(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Cast { ty, .. } => Some(ty),
        Expr::Literal(Value::Int(value)) if i32::try_from(*value).is_ok() => Some("integer"),
        Expr::Literal(Value::Int(_)) => Some("bigint"),
        Expr::Literal(Value::Bytes(_)) => Some("bytea"),
        _ => None,
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

/// Physical scalar built-ins selected after `PostgreSQL` overload resolution has preserved the declared integer width.
pub const TO_HEX_INT4_FUNCTION: &str = "__to_hex_int4";
pub const TO_HEX_INT8_FUNCTION: &str = "__to_hex_int8";
/// Physical marker emitted when declared argument types cannot resolve a built-in overload.
pub const UNDEFINED_FUNCTION_MARKER: &str = "\0uqa.undefined_function:";

/// Return the `PostgreSQL` 18 strictness contract for a built-in scalar call when its implemented overload is known.
#[must_use]
pub fn builtin_scalar_function_strictness(name: &str, argument_count: usize) -> Option<bool> {
    let normalized = normalized_function_name(name);
    match normalized.as_ref() {
        "coalesce" | "greatest" | "least" if argument_count >= 1 => Some(false),
        "nullif" | "concat_op" if argument_count == 2 => Some(false),
        "concat" | "format" | "json_build_array" | "jsonb_build_array" | "json_build_object"
        | "jsonb_build_object" | "num_nulls" | "num_nonnulls" => Some(false),
        "concat_ws" if argument_count >= 1 => Some(false),
        "quote_nullable" | "pg_typeof" | "typeof" if argument_count == 1 => Some(false),
        "array_cat" | "array_append" | "array_prepend" | "array_remove" | "array_positions"
            if argument_count == 2 =>
        {
            Some(false)
        }
        "array_position" if matches!(argument_count, 2 | 3) => Some(false),
        "array_replace" if argument_count == 3 => Some(false),
        "array_fill" if matches!(argument_count, 2 | 3) => Some(false),
        "array_to_string" if argument_count == 3 => Some(false),
        "string_to_array" | "string_to_table" if matches!(argument_count, 2 | 3) => Some(false),
        "overlaps" if argument_count == 4 => Some(false),
        "abs"
        | "acos"
        | "array_dims"
        | "array_ndims"
        | "array_reverse"
        | "ascii"
        | "asin"
        | "atan"
        | "bit_length"
        | "cardinality"
        | "casefold"
        | "cbrt"
        | "ceil"
        | "ceiling"
        | "char_length"
        | "character_length"
        | "chr"
        | "cos"
        | "cosh"
        | "current_schemas"
        | "degrees"
        | "exp"
        | "factorial"
        | "floor"
        | "gamma"
        | "initcap"
        | "isfinite"
        | "json_array_length"
        | "jsonb_array_length"
        | "json_typeof"
        | "jsonb_typeof"
        | "jsonb_pretty"
        | "justify_hours"
        | "length"
        | "lgamma"
        | "ln"
        | "log10"
        | "log2"
        | "lower"
        | "md5"
        | "octet_length"
        | "quote_ident"
        | "quote_literal"
        | "radians"
        | "reverse"
        | "row_to_json"
        | "sign"
        | "sin"
        | "sinh"
        | "sqrt"
        | "tan"
        | "tanh"
        | "to_hex"
        | TO_HEX_INT4_FUNCTION
        | TO_HEX_INT8_FUNCTION
        | "to_json"
        | "to_jsonb"
        | "to_timestamp"
        | "upper"
        | "uuid_extract_timestamp"
        | "uuid_extract_version"
            if argument_count == 1 =>
        {
            Some(true)
        }
        "age" | "btrim" | "ltrim" | "rtrim" | "trim" | "log" | "round" | "trunc"
        | "json_strip_nulls" | "jsonb_strip_nulls"
            if matches!(argument_count, 1 | 2) =>
        {
            Some(true)
        }
        "array_sort" if matches!(argument_count, 1..=3) => Some(true),
        "array_length" | "array_lower" | "array_upper" | "atan2" | "date_part" | "date_trunc"
        | "decode" | "encode" | "extract" | "gcd" | "lcm" | "left" | "mod" | "power" | "pow"
        | "repeat" | "right" | "starts_with" | "position" | "strpos" | "to_char" | "to_date"
        | "to_number" | "trim_array" | "point" | "st_distance" | "st_within"
            if argument_count == 2 =>
        {
            Some(true)
        }
        "like" | "ilike" | "similar_to" if argument_count == 2 => Some(true),
        "like" | "ilike" | "similar_to" if argument_count == 3 => Some(false),
        "array_to_string" if argument_count == 2 => Some(true),
        "substring" | "substr" | "lpad" | "rpad" if matches!(argument_count, 2 | 3) => Some(true),
        "regexp_count" if matches!(argument_count, 2..=4) => Some(true),
        "regexp_instr" if matches!(argument_count, 2..=7) => Some(true),
        "regexp_like" | "regexp_match" | "regexp_matches" if matches!(argument_count, 2 | 3) => {
            Some(true)
        }
        "regexp_replace" if matches!(argument_count, 3..=6) => Some(true),
        "regexp_substr" if matches!(argument_count, 2..=6) => Some(true),
        "replace" | "split_part" | "translate" | "make_date" if argument_count == 3 => Some(true),
        "__between_symmetric" if argument_count == 3 => Some(true),
        "overlay" | "jsonb_set" | "jsonb_insert" if matches!(argument_count, 3 | 4) => Some(true),
        "json_extract_path"
        | "jsonb_extract_path"
        | "json_extract_path_text"
        | "jsonb_extract_path_text"
            if argument_count >= 2 =>
        {
            Some(true)
        }
        "json_contains" | "json_contained_by" | "json_delete_path" | "json_has_key"
        | "json_has_any_key" | "json_has_all_keys" | "jsonb_path_exists" | "jsonpath_exists"
        | "jsonb_path_match" | "jsonpath_match"
            if argument_count == 2 =>
        {
            Some(true)
        }
        "make_timestamp" if matches!(argument_count, 6 | 7) => Some(true),
        "make_interval" if argument_count <= 7 => Some(true),
        "width_bucket" if argument_count == 4 => Some(true),
        "st_dwithin" if matches!(argument_count, 2 | 3) => Some(true),
        _ => None,
    }
}

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
                Ok((Some(arg_name.clone()), eval(value_expr, ctx)?))
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
        return ArrayValue::try_new(schemas.into_iter().map(Value::Str).collect())
            .map(Value::Array)
            .ok_or_else(|| SQLError::TypeMismatch("invalid current_schemas result".into()));
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
        if let Some(positional) = builtin_named_args(lower, &call_args) {
            return eval_scalar_function(lower, &positional);
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

fn builtin_named_args(function: &str, call_args: &[(Option<String>, Value)]) -> Option<Vec<Value>> {
    let names: &[&str] = match function {
        "regexp_count" => match call_args.len() {
            2 => &["string", "pattern"],
            3 => &["string", "pattern", "start"],
            4 => &["string", "pattern", "start", "flags"],
            _ => return None,
        },
        "regexp_like" => match call_args.len() {
            2 => &["string", "pattern"],
            3 => &["string", "pattern", "flags"],
            _ => return None,
        },
        "regexp_substr" => match call_args.len() {
            2 => &["string", "pattern"],
            3 => &["string", "pattern", "start"],
            4 => &["string", "pattern", "start", "N"],
            5 => &["string", "pattern", "start", "N", "flags"],
            6 => &["string", "pattern", "start", "N", "flags", "subexpr"],
            _ => return None,
        },
        "regexp_instr" => match call_args.len() {
            2 => &["string", "pattern"],
            3 => &["string", "pattern", "start"],
            4 => &["string", "pattern", "start", "N"],
            5 => &["string", "pattern", "start", "N", "endoption"],
            6 => &["string", "pattern", "start", "N", "endoption", "flags"],
            7 => &[
                "string",
                "pattern",
                "start",
                "N",
                "endoption",
                "flags",
                "subexpr",
            ],
            _ => return None,
        },
        "regexp_replace" => match call_args.len() {
            3 => &["string", "pattern", "replacement"],
            4 if call_args
                .iter()
                .any(|(name, _)| name.as_deref() == Some("flags")) =>
            {
                &["string", "pattern", "replacement", "flags"]
            }
            4 => &["string", "pattern", "replacement", "start"],
            5 => &["string", "pattern", "replacement", "start", "N"],
            6 => &["string", "pattern", "replacement", "start", "N", "flags"],
            _ => return None,
        },
        "make_interval" => return make_interval_named_args(call_args),
        _ => return None,
    };
    reorder_named_args(call_args, names)
}

fn reorder_named_args(
    call_args: &[(Option<String>, Value)],
    parameter_names: &[&str],
) -> Option<Vec<Value>> {
    if call_args.len() != parameter_names.len() {
        return None;
    }
    let mut slots = vec![None; parameter_names.len()];
    let mut positional_index = 0;
    let mut saw_named = false;
    for (name, value) in call_args {
        let slot = if let Some(name) = name {
            saw_named = true;
            parameter_names
                .iter()
                .position(|candidate| candidate == name)?
        } else {
            if saw_named {
                return None;
            }
            let slot = positional_index;
            positional_index += 1;
            slot
        };
        if slots.get(slot)?.is_some() {
            return None;
        }
        slots[slot] = Some(value.clone());
    }
    slots.into_iter().collect()
}

/// Map `make_interval(name => value, ...)` onto the positional
/// `(years, months, weeks, days, hours, mins, secs)` argument list.
/// Returns `None` when an unknown parameter name appears.
fn make_interval_named_args(call_args: &[(Option<String>, Value)]) -> Option<Vec<Value>> {
    const NAMES: [&str; 7] = ["years", "months", "weeks", "days", "hours", "mins", "secs"];
    let mut positional = vec![Value::Int(0); NAMES.len()];
    let mut positional_index = 0;
    let mut saw_named = false;
    let mut assigned = [false; NAMES.len()];
    for (name, value) in call_args {
        let slot = if let Some(name) = name {
            saw_named = true;
            NAMES.iter().position(|candidate| candidate == name)?
        } else {
            if saw_named {
                return None;
            }
            let slot = positional_index;
            positional_index += 1;
            slot
        };
        if slot >= NAMES.len() || assigned[slot] {
            return None;
        }
        assigned[slot] = true;
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
        Value::FixedChar(_) => "character",
        Value::Bytes(_) => "bytea",
        Value::Temporal(TemporalValue::Interval { .. }) => "interval",
        Value::Temporal(_) => "timestamp",
        Value::Decimal(_) => "numeric",
        Value::Json(_) => "json",
        Value::JsonB(_) => "jsonb",
        Value::Array(_) => "anyarray",
        Value::List(_) => "anyarray",
        Value::Row(_) | Value::Record(_) => "record",
        Value::Map(_) => "jsonb",
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
