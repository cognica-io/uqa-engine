//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection-slot binding for supported scalar predicates.

use uqa_core::Value;
use uqa_sql::expr::cast_value;
use uqa_sql::{SQLError, SQLParam};

use super::ProjectedExpr;
use crate::scalar::scalar_integer_binary_width;
use crate::ScalarExpr;

pub(super) fn compile(
    expression: &ScalarExpr,
    fields: &[String],
    params: &[SQLParam],
) -> Result<Option<ProjectedExpr>, SQLError> {
    let compiled = match expression {
        ScalarExpr::Column(column) => {
            let Some(index) = resolve_unqualified_field(column, fields) else {
                return Ok(None);
            };
            ProjectedExpr::Field(index)
        }
        ScalarExpr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => {
            let Some(index) = resolve_qualified_field(qualifier, column, key, fields) else {
                return Ok(None);
            };
            ProjectedExpr::Field(index)
        }
        ScalarExpr::Literal(value) => ProjectedExpr::Literal(value.clone()),
        ScalarExpr::Param(index) => ProjectedExpr::Literal(parameter(*index, params)?),
        ScalarExpr::Binary { op, lhs, rhs } => {
            let integer_width = scalar_integer_binary_width(lhs, rhs);
            compiled_binary(
                *op,
                require(lhs, fields, params)?,
                require(rhs, fields, params)?,
                integer_width,
            )
        }
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
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } if !distinct && order_by.is_empty() && filter.is_none() => {
            let normalized = name.strip_prefix("pg_catalog.").unwrap_or(name);
            if !normalized.eq_ignore_ascii_case("like") && !normalized.eq_ignore_ascii_case("ilike")
            {
                return Ok(None);
            }
            let [expression, pattern] = args.as_slice() else {
                return Ok(None);
            };
            let pattern = match pattern {
                ScalarExpr::Literal(value) => value.clone(),
                ScalarExpr::Param(index) => parameter(*index, params)?,
                _ => return Ok(None),
            };
            ProjectedExpr::Like {
                expression: Box::new(require(expression, fields, params)?),
                pattern: uqa_sql::expr::CompiledLikePattern::from_value(
                    &pattern,
                    normalized.eq_ignore_ascii_case("ilike"),
                ),
            }
        }
        ScalarExpr::Cast { expr, ty } => {
            let expression = require(expr, fields, params)?;
            match expression {
                // A typed SQL literal is represented as CAST(literal AS type).
                // Evaluate it once while preparing the predicate instead of
                // reparsing (notably DATE/TIMESTAMP text) for every input row.
                ProjectedExpr::Literal(value) if is_integer_type(ty) => ProjectedExpr::Cast {
                    expression: Box::new(ProjectedExpr::Literal(value)),
                    ty: ty.clone(),
                },
                ProjectedExpr::Literal(value) => ProjectedExpr::Literal(cast_value(&value, ty)?),
                expression => ProjectedExpr::Cast {
                    expression: Box::new(expression),
                    ty: ty.clone(),
                },
            }
        }
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

fn is_integer_type(ty: &str) -> bool {
    matches!(
        ty,
        "smallint"
            | "int2"
            | "pg_catalog.int2"
            | "integer"
            | "int"
            | "int4"
            | "serial"
            | "serial4"
            | "pg_catalog.int4"
            | "bigint"
            | "int8"
            | "bigserial"
            | "serial8"
            | "pg_catalog.int8"
    )
}

fn resolve_unqualified_field(column: &str, fields: &[String]) -> Option<usize> {
    if let Some(index) = fields.iter().position(|field| field == column) {
        return Some(index);
    }
    let mut matches = fields.iter().enumerate().filter_map(|(index, field)| {
        field
            .rsplit_once('.')
            .filter(|(_, suffix)| *suffix == column)
            .map(|_| index)
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn resolve_qualified_field(
    qualifier: &str,
    column: &str,
    key: &str,
    fields: &[String],
) -> Option<usize> {
    if !key.is_empty() {
        if let Some(index) = fields.iter().position(|field| field == key) {
            return Some(index);
        }
    }
    let qualified = format!("{qualifier}.{column}");
    fields
        .iter()
        .position(|field| field == &qualified)
        // Storage projections deliberately use unqualified field names. A
        // qualified expression can bind to one of those only when the exact
        // qualifier is absent from the positional schema.
        .or_else(|| fields.iter().position(|field| field == column))
}

fn compiled_binary(
    op: uqa_sql::ast::BinaryOp,
    lhs: ProjectedExpr,
    rhs: ProjectedExpr,
    integer_width: Option<uqa_sql::expr::IntegerWidth>,
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
        integer_width,
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
