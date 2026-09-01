//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection-slot binding for supported scalar predicates.

use uqa_core::Value;
use uqa_sql::expr::cast_value;
use uqa_sql::{SQLError, SQLParam};

use super::{ProjectedExpr, ProjectedIntPredicate};
use crate::scalar::scalar_integer_binary_width;
use crate::{RowSchema, ScalarExpr};

#[expect(
    clippy::too_many_lines,
    reason = "predicate compiler exhaustively accepts or rejects each IR shape"
)]
pub(super) fn compile(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<ProjectedExpr>, SQLError> {
    let compiled = match expression {
        ScalarExpr::Column(column) => {
            let Some(index) = schema.unqualified_position(column) else {
                return Ok(None);
            };
            ProjectedExpr::Field(index)
        }
        ScalarExpr::Position(index) if *index < schema.len() => ProjectedExpr::Field(*index),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            let Some(index) = schema.qualified_position(qualifier, column) else {
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
                require(lhs, schema, params)?,
                require(rhs, schema, params)?,
                integer_width,
            )
        }
        ScalarExpr::UnaryMinus(expression) => {
            ProjectedExpr::UnaryMinus(Box::new(require(expression, schema, params)?))
        }
        ScalarExpr::Not(expression) => {
            ProjectedExpr::Not(Box::new(require(expression, schema, params)?))
        }
        ScalarExpr::And(items) => compiled_and(require_all(items, schema, params)?),
        ScalarExpr::Or(items) => ProjectedExpr::Or(require_all(items, schema, params)?),
        ScalarExpr::IsNull { expr, negated } => ProjectedExpr::IsNull {
            expression: Box::new(require(expr, schema, params)?),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => compiled_between(
            require(expr, schema, params)?,
            require(low, schema, params)?,
            require(high, schema, params)?,
        ),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ProjectedExpr::InList {
            expression: Box::new(require(expr, schema, params)?),
            list: require_all(list, schema, params)?,
            negated: *negated,
        },
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
            ..
        } if !distinct && order_by.is_empty() && filter.is_none() => {
            let normalized = name.strip_prefix("pg_catalog.").unwrap_or(name);
            if !normalized.eq_ignore_ascii_case("like") && !normalized.eq_ignore_ascii_case("ilike")
            {
                return Ok(None);
            }
            let (expression, pattern, escape) = match args.as_slice() {
                [expression, pattern] => (expression, pattern, None),
                [expression, pattern, escape] => (expression, pattern, Some(escape)),
                _ => return Ok(None),
            };
            let pattern = match pattern {
                ScalarExpr::Literal(value) => value.clone(),
                ScalarExpr::Param(index) => parameter(*index, params)?,
                _ => return Ok(None),
            };
            if matches!(&pattern, Value::Null) {
                return Ok(Some(ProjectedExpr::Literal(Value::Null)));
            }
            let escape = match escape {
                Some(ScalarExpr::Literal(value)) => Some(value.clone()),
                Some(ScalarExpr::Param(index)) => Some(parameter(*index, params)?),
                Some(_) => return Ok(None),
                None => None,
            };
            if matches!(&escape, Some(Value::Null)) {
                return Ok(Some(ProjectedExpr::Literal(Value::Null)));
            }
            let pattern_text = uqa_sql::expr::value_to_string(&pattern);
            let escape_text = escape.as_ref().map(uqa_sql::expr::value_to_string);
            ProjectedExpr::Like {
                expression: Box::new(require(expression, schema, params)?),
                pattern: uqa_sql::expr::CompiledLikePattern::with_escape(
                    &pattern_text,
                    normalized.eq_ignore_ascii_case("ilike"),
                    escape_text.as_deref(),
                )?,
            }
        }
        ScalarExpr::Cast { expr, ty } => {
            let expression = require(expr, schema, params)?;
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
        ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Func { .. }
        | ScalarExpr::Array(_)
        | ScalarExpr::Row(_)
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::Case { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => return Ok(None),
    };
    Ok(Some(compiled))
}

fn compiled_and(items: Vec<ProjectedExpr>) -> ProjectedExpr {
    let all_integer_fields = items.iter().all(|item| {
        matches!(
            item,
            ProjectedExpr::IntFieldComparison { .. } | ProjectedExpr::IntFieldBetween { .. }
        )
    });
    if !all_integer_fields {
        return ProjectedExpr::And(items);
    }
    ProjectedExpr::IntFieldConjunction(
        items
            .into_iter()
            .map(|item| match item {
                ProjectedExpr::IntFieldComparison {
                    field,
                    op,
                    literal,
                    field_on_left,
                } => ProjectedIntPredicate::Comparison {
                    field,
                    op,
                    literal,
                    field_on_left,
                },
                ProjectedExpr::IntFieldBetween { field, low, high } => {
                    ProjectedIntPredicate::Between { field, low, high }
                }
                _ => unreachable!("integer predicate conjunction was validated"),
            })
            .collect(),
    )
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
            | "oid"
            | "pg_catalog.oid"
            | "xid"
            | "pg_catalog.xid"
    )
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
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<ProjectedExpr, SQLError> {
    compile(expression, schema, params)?.ok_or_else(|| {
        SQLError::Unsupported("expression cannot use positional predicate evaluation".into())
    })
}

fn require_all(
    expressions: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<ProjectedExpr>, SQLError> {
    expressions
        .iter()
        .map(|expression| require(expression, schema, params))
        .collect()
}

fn parameter(index: usize, params: &[SQLParam]) -> Result<Value, SQLError> {
    match index.checked_sub(1).and_then(|offset| params.get(offset)) {
        Some(SQLParam::Scalar(value) | SQLParam::TypedScalar { value, .. }) => Ok(value.clone()),
        Some(SQLParam::Vector(_) | SQLParam::Tensor(_)) => Err(SQLError::Unsupported(
            "vector parameters require canonical predicate evaluation".into(),
        )),
        None => Err(SQLError::MissingParam(index)),
    }
}
