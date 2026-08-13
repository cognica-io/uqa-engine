//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL comparison, three-valued logic, and numeric arithmetic.

use super::{
    eval, json_delete, time, to_decimal, to_f64, BinaryOp, DecimalValue, EvalContext, Expr, Result,
    ResultRow, SQLError, SQLParam, Value,
};

pub(super) fn eval_binary(
    op: BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    ctx: &EvalContext<'_>,
) -> Result<Value> {
    if let Some(value) = eval_binary_borrowed(op, lhs, rhs, ctx)? {
        return Ok(value);
    }
    let l = eval(lhs, ctx)?;
    let r = eval(rhs, ctx)?;
    eval_binary_values_with_integer_width(op, &l, &r, integer_binary_width(lhs, rhs))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IntegerWidth {
    SmallInt,
    Integer,
    BigInt,
}

#[must_use]
pub fn integer_width_for_literal(value: i64) -> IntegerWidth {
    if i32::try_from(value).is_ok() {
        IntegerWidth::Integer
    } else {
        IntegerWidth::BigInt
    }
}

#[must_use]
pub fn integer_width_for_type(ty: &str) -> Option<IntegerWidth> {
    let ty = ty.trim().to_ascii_lowercase();
    match ty.as_str() {
        "smallint" | "int2" | "pg_catalog.int2" => Some(IntegerWidth::SmallInt),
        "integer" | "int" | "int4" | "serial" | "serial4" | "pg_catalog.int4" => {
            Some(IntegerWidth::Integer)
        }
        "bigint" | "int8" | "bigserial" | "serial8" | "pg_catalog.int8" => {
            Some(IntegerWidth::BigInt)
        }
        _ => None,
    }
}

fn integer_expr_width(expr: &Expr) -> Option<IntegerWidth> {
    match expr {
        Expr::Literal(Value::Int(value)) => Some(integer_width_for_literal(*value)),
        Expr::Cast { ty, .. } => integer_width_for_type(ty),
        Expr::Binary {
            op: BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide,
            lhs,
            rhs,
        } => Some(integer_expr_width(lhs)?.max(integer_expr_width(rhs)?)),
        _ => None,
    }
}

fn integer_binary_width(lhs: &Expr, rhs: &Expr) -> Option<IntegerWidth> {
    Some(integer_expr_width(lhs)?.max(integer_expr_width(rhs)?))
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

/// Evaluate an operator while retaining the integer type selected by SQL
/// operator resolution. The dynamic [`Value`] carrier stores all integers as
/// `i64`, so expression plans pass this width alongside the operands.
pub fn eval_binary_values_with_integer_width(
    op: BinaryOp,
    l: &Value,
    r: &Value,
    integer_width: Option<IntegerWidth>,
) -> Result<Value> {
    let value = eval_binary_values(op, l, r)?;
    let Some(integer_width) = integer_width else {
        return Ok(value);
    };
    let Value::Int(value) = value else {
        return Ok(value);
    };
    let in_range = match integer_width {
        IntegerWidth::SmallInt => i16::try_from(value).is_ok(),
        IntegerWidth::Integer => i32::try_from(value).is_ok(),
        IntegerWidth::BigInt => true,
    };
    if in_range {
        Ok(Value::Int(value))
    } else {
        Err(out_of_range(match integer_width {
            IntegerWidth::SmallInt => "smallint",
            IntegerWidth::Integer => "integer",
            IntegerWidth::BigInt => "bigint",
        }))
    }
}

/// Comparison operators under SQL three-valued logic: any NULL operand
/// makes the result NULL.
pub(super) fn eval_comparison_op(op: BinaryOp, l: &Value, r: &Value) -> Result<Value> {
    Ok(eval_comparison_truth(op, l, r)?
        .map(Value::Bool)
        .unwrap_or(Value::Null))
}

/// Compare two values without allocating an intermediate [`Value::Bool`].
///
/// `None` is SQL UNKNOWN (normally caused by NULL). Predicate executors use
/// this form so comparisons and boolean composition stay in a compact
/// tri-state representation throughout the row-filtering hot path.
#[inline]
pub fn eval_comparison_truth(op: BinaryOp, l: &Value, r: &Value) -> Result<Option<bool>> {
    let out = match op {
        BinaryOp::Equal => values_equal_nullable(l, r),
        BinaryOp::NotEqual => values_equal_nullable(l, r).map(|equal| !equal),
        BinaryOp::Less => compare_nullable(l, r)?.map(|ord| ord.is_lt()),
        BinaryOp::LessEqual => compare_nullable(l, r)?.map(|ord| ord.is_le()),
        BinaryOp::Greater => compare_nullable(l, r)?.map(|ord| ord.is_gt()),
        BinaryOp::GreaterEqual => compare_nullable(l, r)?.map(|ord| ord.is_ge()),
        _ => {
            return Err(SQLError::Internal(format!(
                "non-comparison operator {op:?} reached comparison evaluation"
            )))
        }
    };
    Ok(out)
}

pub(super) enum EvalOperand<'a> {
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

pub(super) fn eval_binary_borrowed(
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

pub(super) fn eval_operand_borrowed<'a>(
    expr: &Expr,
    ctx: &EvalContext<'a>,
) -> Result<Option<EvalOperand<'a>>> {
    match expr {
        Expr::Literal(value) => Ok(Some(EvalOperand::Owned(value.clone()))),
        Expr::Param(i) => match i.checked_sub(1).and_then(|index| ctx.params.get(index)) {
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

pub(super) fn row_column_value<'a>(row: &'a ResultRow, name: &str) -> Option<&'a Value> {
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
        Value::Str(s) | Value::FixedChar(s) => !s.is_empty(),
        _ => true,
    }
}

/// Two-valued equality used where SQL treats a NULL comparison as
/// simply "no match" (CASE base matching, NULLIF, IN-subquery probes).
pub(super) fn values_equal(a: &Value, b: &Value) -> bool {
    values_equal_nullable(a, b) == Some(true)
}

/// Three-valued equality: `None` when either side is NULL (or, for row
/// values, when element NULLs leave the outcome undecided).
pub(super) fn values_equal_nullable(a: &Value, b: &Value) -> Option<bool> {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => None,
        (
            Value::Int(_) | Value::Float(_) | Value::Decimal(_),
            Value::Int(_) | Value::Float(_) | Value::Decimal(_),
        ) => Some(a.cmp(b) == std::cmp::Ordering::Equal),
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
        (Value::FixedChar(x), Value::FixedChar(y)) => {
            Some(x.trim_end_matches(' ') == y.trim_end_matches(' '))
        }
        (Value::FixedChar(x), Value::Str(y)) | (Value::Str(y), Value::FixedChar(x)) => {
            Some(x.trim_end_matches(' ') == y.trim_end_matches(' '))
        }
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

pub(super) fn compare(a: &Value, b: &Value) -> Result<std::cmp::Ordering> {
    Ok(compare_nullable(a, b)?.unwrap_or(std::cmp::Ordering::Equal))
}

/// Three-valued ordering: `None` when a NULL operand (or an undecided
/// NULL row element) leaves the comparison unknown.
pub(super) fn compare_nullable(a: &Value, b: &Value) -> Result<Option<std::cmp::Ordering>> {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => Ok(None),
        (
            Value::Int(_) | Value::Float(_) | Value::Decimal(_),
            Value::Int(_) | Value::Float(_) | Value::Decimal(_),
        ) => Ok(Some(a.cmp(b))),
        (Value::Bool(x), Value::Decimal(y)) => Ok(Some(DecimalValue::from_bool(*x).cmp(y))),
        (Value::Decimal(x), Value::Bool(y)) => Ok(Some(x.cmp(&DecimalValue::from_bool(*y)))),
        (Value::Str(x), Value::Str(y)) => Ok(Some(x.cmp(y))),
        (Value::FixedChar(x), Value::FixedChar(y)) => {
            Ok(Some(x.trim_end_matches(' ').cmp(y.trim_end_matches(' '))))
        }
        (Value::FixedChar(x), Value::Str(y)) => {
            Ok(Some(x.trim_end_matches(' ').cmp(y.trim_end_matches(' '))))
        }
        (Value::Str(x), Value::FixedChar(y)) => {
            Ok(Some(x.trim_end_matches(' ').cmp(y.trim_end_matches(' '))))
        }
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

pub(super) fn arith(a: &Value, b: &Value, op: BinaryOp) -> Result<Value> {
    // SQL three-valued logic: NULL `op` anything == NULL.
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return Ok(Value::Null);
    }
    // Integer x integer is the overwhelmingly common analytical path.
    // Resolve it before probing unrelated temporal / decimal / floating
    // representations, while retaining PostgreSQL overflow behavior. The
    // caller applies the SQL operator's int2/int4/int8 result width after this
    // carrier-level i64 operation.
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
            _ => {
                return Err(SQLError::Internal(format!(
                    "non-arithmetic operator {op:?} reached integer arithmetic"
                )))
            }
        };
        return out.map(Value::Int).ok_or_else(|| out_of_range("bigint"));
    }
    if matches!(op, BinaryOp::Subtract)
        && matches!(a, Value::JsonB(_) | Value::Map(_) | Value::List(_))
    {
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
        _ => {
            return Err(SQLError::Internal(format!(
                "non-arithmetic operator {op:?} reached floating arithmetic"
            )))
        }
    };
    Ok(Value::Float(result))
}

pub(super) fn decimal_arith(a: &Value, b: &Value, op: BinaryOp) -> Result<Value> {
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
            left.checked_div_postgres(&right)
        }
        _ => {
            return Err(SQLError::Internal(format!(
                "non-arithmetic operator {op:?} reached decimal arithmetic"
            )))
        }
    }
    .ok_or_else(|| out_of_range("numeric"))?;
    Ok(Value::Decimal(value))
}
