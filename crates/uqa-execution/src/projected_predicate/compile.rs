//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection-slot binding for supported scalar predicates.

use uqa_core::Value;
use uqa_sql::{SQLError, SQLParam};

use super::ProjectedExpr;
use crate::ScalarExpr;

pub(super) fn compile(
    expression: &ScalarExpr,
    fields: &[String],
    params: &[SQLParam],
) -> Result<Option<ProjectedExpr>, SQLError> {
    let compiled = match expression {
        ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } => {
            let Some(index) = fields.iter().position(|field| field == column) else {
                return Ok(None);
            };
            ProjectedExpr::Field(index)
        }
        ScalarExpr::Literal(value) => ProjectedExpr::Literal(value.clone()),
        ScalarExpr::Param(index) => ProjectedExpr::Literal(parameter(*index, params)?),
        ScalarExpr::Binary { op, lhs, rhs } => compiled_binary(
            *op,
            require(lhs, fields, params)?,
            require(rhs, fields, params)?,
        ),
        ScalarExpr::Not(expression) => {
            ProjectedExpr::Not(Box::new(require(expression, fields, params)?))
        }
        ScalarExpr::And(items) => ProjectedExpr::And(require_all(items, fields, params)?),
        ScalarExpr::Or(items) => ProjectedExpr::Or(require_all(items, fields, params)?),
        ScalarExpr::IsNull { expr, negated } => ProjectedExpr::IsNull {
            expression: Box::new(require(expr, fields, params)?),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => compiled_between(
            require(expr, fields, params)?,
            require(low, fields, params)?,
            require(high, fields, params)?,
        ),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ProjectedExpr::InList {
            expression: Box::new(require(expr, fields, params)?),
            list: require_all(list, fields, params)?,
            negated: *negated,
        },
        ScalarExpr::Cast { expr, ty } => ProjectedExpr::Cast {
            expression: Box::new(require(expr, fields, params)?),
            ty: ty.clone(),
        },
        ScalarExpr::Star
        | ScalarExpr::Func { .. }
        | ScalarExpr::Array(_)
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::Case { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => return Ok(None),
    };
    Ok(Some(compiled))
}

fn compiled_binary(
    op: uqa_sql::ast::BinaryOp,
    lhs: ProjectedExpr,
    rhs: ProjectedExpr,
) -> ProjectedExpr {
    use uqa_sql::ast::BinaryOp;

    if matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        match (&lhs, &rhs) {
            (ProjectedExpr::Field(field), ProjectedExpr::Literal(Value::Int(literal))) => {
                return ProjectedExpr::IntFieldComparison {
                    field: *field,
                    op,
                    literal: *literal,
                    field_on_left: true,
                };
            }
            (ProjectedExpr::Literal(Value::Int(literal)), ProjectedExpr::Field(field)) => {
                return ProjectedExpr::IntFieldComparison {
                    field: *field,
                    op,
                    literal: *literal,
                    field_on_left: false,
                };
            }
            _ => {}
        }
    }
    ProjectedExpr::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

fn compiled_between(
    expression: ProjectedExpr,
    low: ProjectedExpr,
    high: ProjectedExpr,
) -> ProjectedExpr {
    if let (
        ProjectedExpr::Field(field),
        ProjectedExpr::Literal(Value::Int(low)),
        ProjectedExpr::Literal(Value::Int(high)),
    ) = (&expression, &low, &high)
    {
        return ProjectedExpr::IntFieldBetween {
            field: *field,
            low: *low,
            high: *high,
        };
    }
    ProjectedExpr::Between {
        expression: Box::new(expression),
        low: Box::new(low),
        high: Box::new(high),
    }
}

fn require(
    expression: &ScalarExpr,
    fields: &[String],
    params: &[SQLParam],
) -> Result<ProjectedExpr, SQLError> {
    compile(expression, fields, params)?.ok_or_else(|| {
        SQLError::Unsupported("expression cannot use positional predicate evaluation".into())
    })
}

fn require_all(
    expressions: &[ScalarExpr],
    fields: &[String],
    params: &[SQLParam],
) -> Result<Vec<ProjectedExpr>, SQLError> {
    expressions
        .iter()
        .map(|expression| require(expression, fields, params))
        .collect()
}

fn parameter(index: usize, params: &[SQLParam]) -> Result<Value, SQLError> {
    match index.checked_sub(1).and_then(|offset| params.get(offset)) {
        Some(SQLParam::Scalar(value)) => Ok(value.clone()),
        Some(SQLParam::Vector(_) | SQLParam::Tensor(_)) => Err(SQLError::Unsupported(
            "vector parameters require canonical predicate evaluation".into(),
        )),
        None => Err(SQLError::MissingParam(index)),
    }
}
