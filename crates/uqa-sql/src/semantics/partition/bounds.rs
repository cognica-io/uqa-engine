//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` partition bound transformation (`transformPartitionBound`). Every declared value is analyzed as a partition bound expression, coerced to its partition key type with assignment semantics, and evaluated once, so a stored bound holds typed constants that routing compares directly and a later catalog change such as `ALTER TYPE ... RENAME VALUE` cannot reinterpret.

use super::key::{key_columns, KeyColumn};
use super::PartitionContext;
use crate::ast::{Expr, PartitionBound, PartitionRangeDatum, PartitionSpec, PartitionStrategy};
use crate::ir::ScalarExpr;
use crate::schema::SchemaExpressionCatalog;
use crate::SQLError;
use uqa_core::Value;

/// Transform a declared bound for a new partition of `parent` into its stored form.
pub fn transform_partition_bound(
    context: &PartitionContext<'_>,
    parent: &str,
    bound: &PartitionBound,
) -> Result<PartitionBound, SQLError> {
    let (spec, keys) = parent_partition_key(context, parent)?;
    match (spec.strategy, bound) {
        (PartitionStrategy::Hash, PartitionBound::Default) => Err(invalid_table_definition(
            "a hash-partitioned table may not have a default partition",
        )),
        (_, PartitionBound::Default) => Ok(PartitionBound::Default),
        (PartitionStrategy::Hash, PartitionBound::Hash { modulus, remainder }) => {
            if *modulus <= 0 {
                return Err(invalid_table_definition(
                    "modulus for hash partition must be an integer value greater than zero",
                ));
            }
            if *remainder < 0 {
                return Err(invalid_table_definition(
                    "remainder for hash partition must be an integer value greater than or equal to zero",
                ));
            }
            if remainder >= modulus {
                return Err(invalid_table_definition(
                    "remainder for hash partition must be less than modulus",
                ));
            }
            Ok(bound.clone())
        }
        (PartitionStrategy::Hash, _) => Err(invalid_table_definition(
            "invalid bound specification for a hash partition",
        )),
        (PartitionStrategy::List, PartitionBound::List(values)) => {
            let [key] = keys.as_slice() else {
                return Err(SQLError::Internal(
                    "LIST partitioned table has more than one partition key".into(),
                ));
            };
            let mut transformed: Vec<Value> = Vec::with_capacity(values.len());
            for expression in values {
                let value = transform_value(context, expression, &key.name, key)?;
                // Equal constants appear once, as `transformPartitionBound` removes duplicates.
                if !transformed.contains(&value) {
                    transformed.push(value);
                }
            }
            Ok(PartitionBound::List(
                transformed.into_iter().map(Expr::Literal).collect(),
            ))
        }
        (PartitionStrategy::List, _) => Err(invalid_table_definition(
            "invalid bound specification for a list partition",
        )),
        (PartitionStrategy::Range, PartitionBound::Range { lower, upper }) => {
            if lower.len() != keys.len() {
                return Err(invalid_table_definition(
                    "FROM must specify exactly one value per partitioning column",
                ));
            }
            if upper.len() != keys.len() {
                return Err(invalid_table_definition(
                    "TO must specify exactly one value per partitioning column",
                ));
            }
            Ok(PartitionBound::Range {
                lower: transform_range_datums(context, lower, &keys)?,
                upper: transform_range_datums(context, upper, &keys)?,
            })
        }
        (PartitionStrategy::Range, _) => Err(invalid_table_definition(
            "invalid bound specification for a range partition",
        )),
    }
}

pub(super) fn parent_partition_key(
    context: &PartitionContext<'_>,
    parent: &str,
) -> Result<(PartitionSpec, Vec<KeyColumn>), SQLError> {
    let hierarchy = context
        .catalog
        .try_table_hierarchy(parent)
        .map_err(|error| SQLError::Internal(format!("read parent partition metadata: {error}")))?;
    let spec = hierarchy.partition_spec.ok_or_else(|| SQLError::Routine {
        sqlstate: "42809".into(),
        message: format!("relation \"{parent}\" is not partitioned"),
    })?;
    let columns = context
        .catalog
        .try_describe_table(parent)
        .map_err(|error| SQLError::Internal(format!("read partition row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(parent.to_string()))?;
    let keys = key_columns(context.types, context.expressions, &spec, &columns)?;
    Ok((spec, keys))
}

/// `transformPartitionRangeBounds`: `MINVALUE` and `MAXVALUE` pass through, NULL is rejected, and infinite bounds must continue to the last column.
fn transform_range_datums(
    context: &PartitionContext<'_>,
    datums: &[PartitionRangeDatum],
    keys: &[KeyColumn],
) -> Result<Vec<PartitionRangeDatum>, SQLError> {
    let expression_names = keys
        .iter()
        .filter(|key| key.expression)
        .map(|key| key.name.as_str())
        .collect::<Vec<_>>();
    // PostgreSQL names an expression key by the next unused key expression, which advances only for value datums.
    let mut next_expression = 0;
    let mut result = Vec::with_capacity(datums.len());
    for (position, datum) in datums.iter().enumerate() {
        let PartitionRangeDatum::Value(expression) = datum else {
            result.push(datum.clone());
            continue;
        };
        let key = &keys[position];
        let name = if key.expression {
            let name = expression_names.get(next_expression).ok_or_else(|| {
                SQLError::Internal("partition range bound names a missing key expression".into())
            })?;
            next_expression += 1;
            *name
        } else {
            key.name.as_str()
        };
        let value = transform_value(context, expression, name, key)?;
        if matches!(value, Value::Null) {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: "cannot specify NULL in range bound".into(),
            });
        }
        result.push(PartitionRangeDatum::Value(Expr::Literal(value)));
    }
    validate_infinite_bounds(&result)?;
    Ok(result)
}

fn validate_infinite_bounds(datums: &[PartitionRangeDatum]) -> Result<(), SQLError> {
    let mut infinite = None;
    for datum in datums {
        match (infinite, datum) {
            (None, PartitionRangeDatum::Value(_))
            | (Some(true), PartitionRangeDatum::MinValue)
            | (Some(false), PartitionRangeDatum::MaxValue) => {}
            (None, PartitionRangeDatum::MinValue) => infinite = Some(true),
            (None, PartitionRangeDatum::MaxValue) => infinite = Some(false),
            (Some(minimum), _) => {
                let bound = if minimum { "MINVALUE" } else { "MAXVALUE" };
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: format!("every bound following {bound} must also be {bound}"),
                });
            }
        }
    }
    Ok(())
}

/// `transformPartitionBoundValue`: analyze, coerce to the key type in assignment context, and evaluate once.
fn transform_value(
    context: &PartitionContext<'_>,
    expression: &Expr,
    column: &str,
    key: &KeyColumn,
) -> Result<Value, SQLError> {
    let plan = crate::plan::ExpressionPlan::lower(expression.clone());
    validate_bound_expression(context.schema, &plan.scalar)?;
    let source = crate::type_resolution::common_context_expression_type(
        &plan.scalar,
        &crate::RowSchema::default(),
        &[],
        Some(context.types),
    )?;
    if let Some(source) = source.as_ref() {
        if !crate::type_resolution::assignment_type_compatible(source, &key.ty) {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "specified value cannot be cast to type {} for column \"{column}\"",
                    format_type(context, &key.ty)?
                ),
            });
        }
    }
    let value = context.expressions.evaluate_bound(expression, &[])?;
    crate::assignment::conversion::coerce_assignment_value(
        context.assignment,
        value,
        &key.ty,
        source.as_ref(),
    )
}

/// `format_type_be`: the catalog's visible spelling of a type, without type modifiers.
pub(super) fn format_type(
    context: &PartitionContext<'_>,
    ty: &crate::ast::ColumnType,
) -> Result<String, SQLError> {
    let oid = crate::catalog::type_metadata::pg_type_oid(ty);
    Ok(context
        .assignment
        .resolve_regtype_output(&crate::ast::ColumnType::Regtype, oid)
        .map_err(SQLError::Internal)?
        .unwrap_or_else(|| ty.regtype_name()))
}

/// Reject what parse analysis disallows in a partition bound, reporting the first violation in analysis order: a call's arguments before the call itself, and a subquery before its body.
fn validate_bound_expression(
    catalog: &dyn SchemaExpressionCatalog,
    expression: &ScalarExpr,
) -> Result<(), SQLError> {
    // A bare `*` only appears as the argument of `count(*)`, which parse analysis reports as an aggregate.
    match expression {
        ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_) => {
            return Err(feature_not_supported(
                "cannot use column reference in partition bound expression",
            ))
        }
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => {
            return Err(feature_not_supported(
                "cannot use subquery in partition bound",
            ))
        }
        ScalarExpr::Param(index) => {
            return Err(SQLError::Routine {
                sqlstate: "42P02".into(),
                message: format!("there is no parameter ${index}"),
            })
        }
        _ => {}
    }
    let mut root = true;
    expression.try_visit(&mut |node| {
        if std::mem::take(&mut root) {
            return Ok(true);
        }
        validate_bound_expression(catalog, node)?;
        Ok::<_, SQLError>(false)
    })?;
    match expression {
        ScalarExpr::WindowCall { .. } => Err(SQLError::Routine {
            sqlstate: "42P20".into(),
            message: "window functions are not allowed in partition bound".into(),
        }),
        ScalarExpr::Func { .. }
            if crate::semantics::aggregates::is_aggregate(catalog, expression) =>
        {
            Err(SQLError::Routine {
                sqlstate: "42803".into(),
                message: "aggregate functions are not allowed in partition bound".into(),
            })
        }
        ScalarExpr::Func { .. }
            if crate::semantics::sets::validation::expression_may_return_set(
                catalog,
                catalog,
                expression,
                &crate::RowSchema::default(),
                &[],
            )? =>
        {
            Err(feature_not_supported(
                "set-returning functions are not allowed in partition bound",
            ))
        }
        _ => Ok(()),
    }
}

fn feature_not_supported(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "0A000".into(),
        message: message.into(),
    }
}

fn invalid_table_definition(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P16".into(),
        message: message.into(),
    }
}
