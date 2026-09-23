//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL comparison, three-valued logic, and numeric arithmetic.

use super::{eval, time, BinaryOp, EvalContext, Expr, Result, SQLError, SQLParam, Value};

use uqa_core::memory::{Produced, ProductionControl};

mod comparison;
#[cfg(test)]
mod production_tests;

pub use comparison::{
    compare_nullable_with_control, compare_with_control, eval_comparison_truth,
    eval_comparison_truth_with_control, values_equal_nullable_with_control,
    values_equal_with_control,
};
pub(super) use comparison::{eval_comparison_op, values_equal, values_equal_nullable};

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
    if is_arithmetic(op) && real_expr(lhs, ctx.params) && real_expr(rhs, ctx.params) {
        return super::eval_float_arithmetic(op, &l, &r, super::FloatWidth::Real);
    }
    eval_binary_values_with_integer_width(op, &l, &r, integer_binary_width(lhs, rhs))
}

pub(super) fn is_arithmetic(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
    )
}

fn real_expr(expression: &Expr, params: &[SQLParam]) -> bool {
    match expression {
        Expr::Cast { ty, .. } | Expr::TypedLiteral { ty, .. } => {
            matches!(
                crate::ast::ColumnType::from_sql_name(ty),
                Ok(crate::ast::ColumnType::Real)
            )
        }
        Expr::Param(index) => index
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .and_then(SQLParam::declared_scalar_type)
            .is_some_and(|ty| matches!(ty, crate::ast::ColumnType::Real)),
        Expr::UnaryMinus(inner) => real_expr(inner, params),
        Expr::Binary { op, lhs, rhs } if is_arithmetic(*op) => {
            real_expr(lhs, params) && real_expr(rhs, params)
        }
        _ => false,
    }
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
    let ty = ty.trim();
    [
        (
            IntegerWidth::SmallInt,
            &["smallint", "int2", "pg_catalog.int2"][..],
        ),
        (
            IntegerWidth::Integer,
            &[
                "integer",
                "int",
                "int4",
                "serial",
                "serial4",
                "pg_catalog.int4",
            ][..],
        ),
        (
            IntegerWidth::BigInt,
            &["bigint", "int8", "bigserial", "serial8", "pg_catalog.int8"][..],
        ),
    ]
    .into_iter()
    .find_map(|(width, names)| {
        names
            .iter()
            .any(|name| ty.eq_ignore_ascii_case(name))
            .then_some(width)
    })
}

fn integer_expr_width(expr: &Expr) -> Option<IntegerWidth> {
    match expr {
        Expr::Literal(Value::Int(value)) => Some(integer_width_for_literal(*value)),
        Expr::Cast { ty, .. } | Expr::TypedLiteral { ty, .. } => integer_width_for_type(ty),
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
    eval_binary_values_with_control(op, l, r, &ProductionControl::uncontrolled()).map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary binary result has no reservation")
    })
}

/// Evaluate the existing binary operator while owning every value producer and comparison workspace under one allowance.
pub fn eval_binary_values_with_control(
    op: BinaryOp,
    l: &Value,
    r: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    match op {
        BinaryOp::Equal
        | BinaryOp::NotEqual
        | BinaryOp::Less
        | BinaryOp::LessEqual
        | BinaryOp::Greater
        | BinaryOp::GreaterEqual => {
            let value = eval_comparison_truth_with_control(op, l, r, control)?
                .map(Value::Bool)
                .unwrap_or(Value::Null);
            control
                .finish(value, control.empty_reservation())
                .map_err(Into::into)
        }
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
            arith(l, r, op, control)
        }
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
    eval_binary_values_with_integer_width_with_control(
        op,
        l,
        r,
        integer_width,
        &ProductionControl::uncontrolled(),
    )
    .map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary width-checked result has no reservation")
    })
}

/// Preserve the selected integer width without separating an allocated result from its owner on errors.
pub fn eval_binary_values_with_integer_width_with_control(
    op: BinaryOp,
    l: &Value,
    r: &Value,
    integer_width: Option<IntegerWidth>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let result = eval_binary_values_with_control(op, l, r, control)?;
    let Some(integer_width) = integer_width else {
        return Ok(result);
    };
    let Value::Int(value) = *result else {
        return Ok(result);
    };
    let in_range = match integer_width {
        IntegerWidth::SmallInt => i16::try_from(value).is_ok(),
        IntegerWidth::Integer => i32::try_from(value).is_ok(),
        IntegerWidth::BigInt => true,
    };
    if in_range {
        Ok(result)
    } else {
        Err(out_of_range(match integer_width {
            IntegerWidth::SmallInt => "smallint",
            IntegerWidth::Integer => "integer",
            IntegerWidth::BigInt => "bigint",
        }))
    }
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
            Some(SQLParam::Scalar(value) | SQLParam::TypedScalar { value, .. }) => {
                Ok(Some(EvalOperand::Borrowed(value)))
            }
            Some(SQLParam::Vector(_)) | Some(SQLParam::Tensor(_)) => Ok(None),
            None => Err(SQLError::MissingParam(*i)),
        },
        Expr::Column(name) => {
            if ctx.row_lookup()?.column_is_ambiguous(name) {
                return Err(SQLError::AmbiguousColumn(name.clone()));
            }
            Ok(Some(match ctx.row_lookup()?.column(name) {
                Some(value) => EvalOperand::Borrowed(value),
                None => EvalOperand::Owned(Value::Null),
            }))
        }
        Expr::QualifiedColumn { qualifier, column } => {
            if ctx
                .row_lookup()?
                .qualified_column_is_ambiguous(qualifier, column)
            {
                return Err(SQLError::AmbiguousColumn(format!("{qualifier}.{column}")));
            }
            Ok(Some(
                match ctx.row_lookup()?.qualified_column(qualifier, column) {
                    Some(value) => EvalOperand::Borrowed(value),
                    None => EvalOperand::Owned(Value::Null),
                },
            ))
        }
        _ => Ok(None),
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
        Value::Decimal(d) => !d.is_zero(),
        Value::Str(s) | Value::FixedChar(s) => !s.is_empty(),
        _ => true,
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

fn arith(
    a: &Value,
    b: &Value,
    op: BinaryOp,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    // SQL three-valued logic: NULL `op` anything == NULL.
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return control
            .finish(Value::Null, control.empty_reservation())
            .map_err(Into::into);
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
        let value = out.map(Value::Int).ok_or_else(|| out_of_range("bigint"))?;
        return control
            .finish(value, control.empty_reservation())
            .map_err(Into::into);
    }
    if matches!(op, BinaryOp::Subtract)
        && matches!(a, Value::JsonB(_) | Value::Map(_) | Value::List(_))
    {
        if let Some(value) = super::json::json_delete_values_with_control(a, b, control)? {
            return Ok(value);
        }
    }
    if matches!(a, Value::Temporal(_)) || matches!(b, Value::Temporal(_)) {
        let value = time::temporal_arith_with_control(a, b, op, control)?;
        return control
            .finish(value, control.empty_reservation())
            .map_err(Into::into);
    }
    let has_decimal = matches!(a, Value::Decimal(_)) || matches!(b, Value::Decimal(_));
    let has_float = matches!(a, Value::Float(_)) || matches!(b, Value::Float(_));
    // PostgreSQL numeric promotion: double precision wins mixed
    // float/numeric arithmetic. Exact decimal arithmetic only applies
    // when no float operand is involved.
    if has_decimal && !has_float {
        return decimal_arith(a, b, op, control);
    }
    let value = super::eval_float_arithmetic_with_control(
        op,
        a,
        b,
        super::FloatWidth::DoublePrecision,
        control,
    )?;
    control
        .finish(value, control.empty_reservation())
        .map_err(Into::into)
}

fn decimal_arith(
    a: &Value,
    b: &Value,
    op: BinaryOp,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let left = super::conversion::to_decimal_with_control(a, control)?;
    let right = super::conversion::to_decimal_with_control(b, control)?;
    let value = match op {
        BinaryOp::Add => left.checked_add_with_control(&right, control)?,
        BinaryOp::Subtract => left.checked_sub_with_control(&right, control)?,
        BinaryOp::Multiply => left.checked_mul_with_control(&right, control)?,
        BinaryOp::Divide => {
            if right.is_zero() {
                return Err(division_by_zero());
            }
            left.checked_div_postgres_with_control(&right, control)?
        }
        _ => {
            return Err(SQLError::Internal(format!(
                "non-arithmetic operator {op:?} reached decimal arithmetic"
            )))
        }
    }
    .ok_or_else(|| out_of_range("numeric"))?;
    let (value, memory) = value.into_parts();
    control
        .finish(Value::Decimal(value), memory)
        .map_err(Into::into)
}
