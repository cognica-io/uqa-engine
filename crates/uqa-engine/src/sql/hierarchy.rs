//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declarative partition routing over durable hierarchy metadata.

use super::{Document, Engine, SQLError, SQLParam, Value};
use std::cmp::Ordering;

pub(in crate::sql) fn validate_new_partition_bound(
    engine: &Engine,
    parent: &str,
    bound: &uqa_sql::ast::PartitionBound,
) -> Result<(), SQLError> {
    let hierarchy = engine
        .try_table_hierarchy(parent)
        .map_err(|error| SQLError::Internal(format!("read parent partition metadata: {error}")))?;
    let spec = hierarchy
        .partition_spec
        .as_ref()
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("relation \"{parent}\" is not partitioned"),
        })?;
    validate_partition_bound_width(spec, bound)?;
    if let uqa_sql::ast::PartitionBound::Range { lower, upper } = bound {
        if compare_partition_points(engine, lower, upper)? != Ordering::Less {
            return Err(invalid_partition_bound(
                "empty range bound specified for partition",
            ));
        }
    }
    for sibling in engine.direct_hierarchy_children(parent)? {
        let sibling_hierarchy = engine
            .try_table_hierarchy(&sibling)
            .map_err(|error| SQLError::Internal(format!("read sibling partition: {error}")))?;
        let Some(sibling_bound) = sibling_hierarchy.partition_bound.as_ref() else {
            continue;
        };
        if partition_bounds_overlap(engine, bound, sibling_bound)? {
            return Err(invalid_partition_bound(format!(
                "partition would overlap partition \"{sibling}\""
            )));
        }
    }
    Ok(())
}

fn validate_partition_bound_width(
    spec: &uqa_sql::ast::PartitionSpec,
    bound: &uqa_sql::ast::PartitionBound,
) -> Result<(), SQLError> {
    use uqa_sql::ast::PartitionBound;
    match bound {
        PartitionBound::List(_) if spec.keys.len() != 1 => Err(invalid_partition_bound(
            "cannot use list partition bounds with more than one partition key",
        )),
        PartitionBound::Range { lower, upper }
            if lower.len() != spec.keys.len() || upper.len() != spec.keys.len() =>
        {
            Err(invalid_partition_bound(
                "partition bound has the wrong number of columns",
            ))
        }
        _ => Ok(()),
    }
}

fn partition_bounds_overlap(
    engine: &Engine,
    left: &uqa_sql::ast::PartitionBound,
    right: &uqa_sql::ast::PartitionBound,
) -> Result<bool, SQLError> {
    use uqa_sql::ast::PartitionBound;
    match (left, right) {
        (PartitionBound::Default, PartitionBound::Default) => Ok(true),
        (PartitionBound::Default, _) | (_, PartitionBound::Default) => Ok(false),
        (PartitionBound::List(left), PartitionBound::List(right)) => {
            let left = evaluate_bound_values(engine, left)?;
            let right = evaluate_bound_values(engine, right)?;
            Ok(left.iter().any(|value| right.contains(value)))
        }
        (
            PartitionBound::Range {
                lower: left_lower,
                upper: left_upper,
            },
            PartitionBound::Range {
                lower: right_lower,
                upper: right_upper,
            },
        ) => Ok(
            compare_partition_points(engine, left_lower, right_upper)? == Ordering::Less
                && compare_partition_points(engine, right_lower, left_upper)? == Ordering::Less,
        ),
        (
            PartitionBound::Hash {
                modulus: left_modulus,
                remainder: left_remainder,
            },
            PartitionBound::Hash {
                modulus: right_modulus,
                remainder: right_remainder,
            },
        ) => Ok((left_remainder - right_remainder)
            .rem_euclid(greatest_common_divisor(*left_modulus, *right_modulus))
            == 0),
        _ => Err(SQLError::Internal(
            "partition siblings use different bound strategies".into(),
        )),
    }
}

fn evaluate_bound_values(
    engine: &Engine,
    expressions: &[uqa_sql::ast::Expr],
) -> Result<Vec<Value>, SQLError> {
    expressions
        .iter()
        .map(|expression| super::scalar::eval_lowered_expression(engine, expression, None, &[]))
        .collect()
}

fn compare_partition_points(
    engine: &Engine,
    left: &[uqa_sql::ast::PartitionRangeDatum],
    right: &[uqa_sql::ast::PartitionRangeDatum],
) -> Result<Ordering, SQLError> {
    if left.len() != right.len() {
        return Err(invalid_partition_bound(
            "partition range points have different widths",
        ));
    }
    for (left, right) in left.iter().zip(right) {
        let ordering = match (left, right) {
            (
                uqa_sql::ast::PartitionRangeDatum::MinValue,
                uqa_sql::ast::PartitionRangeDatum::MinValue,
            )
            | (
                uqa_sql::ast::PartitionRangeDatum::MaxValue,
                uqa_sql::ast::PartitionRangeDatum::MaxValue,
            ) => Ordering::Equal,
            (uqa_sql::ast::PartitionRangeDatum::MinValue, _)
            | (_, uqa_sql::ast::PartitionRangeDatum::MaxValue) => Ordering::Less,
            (uqa_sql::ast::PartitionRangeDatum::MaxValue, _)
            | (_, uqa_sql::ast::PartitionRangeDatum::MinValue) => Ordering::Greater,
            (
                uqa_sql::ast::PartitionRangeDatum::Value(left),
                uqa_sql::ast::PartitionRangeDatum::Value(right),
            ) => super::scalar::eval_lowered_expression(engine, left, None, &[])?.cmp(
                &super::scalar::eval_lowered_expression(engine, right, None, &[])?,
            ),
        };
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
}

const fn greatest_common_divisor(mut left: i32, mut right: i32) -> i32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.abs()
}

fn invalid_partition_bound(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P17".into(),
        message: message.into(),
    }
}

pub(in crate::sql) fn partition_insert_target(
    engine: &Engine,
    requested_table: &str,
    document: &Document,
    params: &[SQLParam],
    include_descendants: bool,
) -> Result<String, SQLError> {
    let table = engine
        .try_resolve_table_name(requested_table)
        .map_err(|error| SQLError::Internal(format!("resolve INSERT table: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(requested_table.to_string()))?;
    let hierarchy = engine
        .try_table_hierarchy(&table)
        .map_err(|error| SQLError::Internal(format!("read partition metadata: {error}")))?;
    if hierarchy.partition_bound.is_some() {
        let parent = hierarchy.parents.first().ok_or_else(|| {
            SQLError::Internal(format!("partition `{table}` has no parent relation"))
        })?;
        let selected = select_direct_partition(engine, parent, document, params)?;
        if selected.as_deref() != Some(table.as_str()) {
            return Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: format!(
                    "new row for relation \"{requested_table}\" violates partition constraint"
                ),
            });
        }
    }
    if let Some(spec) = hierarchy.partition_spec.as_ref() {
        if !include_descendants {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot insert into partitioned table \"{requested_table}\""),
            });
        }
        let child = select_direct_partition_with_spec(engine, &table, spec, document, params)?
            .ok_or_else(|| no_partition_for_row(requested_table))?;
        return route_partition_tree(engine, &child, document, params);
    }
    Ok(table)
}

fn route_partition_tree(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
) -> Result<String, SQLError> {
    let hierarchy = engine
        .try_table_hierarchy(table)
        .map_err(|error| SQLError::Internal(format!("read partition metadata: {error}")))?;
    let Some(spec) = hierarchy.partition_spec.as_ref() else {
        return Ok(table.to_string());
    };
    let child = select_direct_partition_with_spec(engine, table, spec, document, params)?
        .ok_or_else(|| no_partition_for_row(table))?;
    route_partition_tree(engine, &child, document, params)
}

fn select_direct_partition(
    engine: &Engine,
    parent: &str,
    document: &Document,
    params: &[SQLParam],
) -> Result<Option<String>, SQLError> {
    let hierarchy = engine
        .try_table_hierarchy(parent)
        .map_err(|error| SQLError::Internal(format!("read parent partition metadata: {error}")))?;
    let spec = hierarchy.partition_spec.as_ref().ok_or_else(|| {
        SQLError::Internal(format!("partition parent `{parent}` has no partition key"))
    })?;
    select_direct_partition_with_spec(engine, parent, spec, document, params)
}

fn select_direct_partition_with_spec(
    engine: &Engine,
    parent: &str,
    spec: &uqa_sql::ast::PartitionSpec,
    document: &Document,
    params: &[SQLParam],
) -> Result<Option<String>, SQLError> {
    let keys = evaluate_partition_keys(engine, parent, &spec.keys, document, params)?;
    let mut default = None;
    for child in engine.direct_hierarchy_children(parent)? {
        let child_hierarchy = engine
            .try_table_hierarchy(&child)
            .map_err(|error| SQLError::Internal(format!("read child partition: {error}")))?;
        let Some(bound) = child_hierarchy.partition_bound.as_ref() else {
            continue;
        };
        if matches!(bound, uqa_sql::ast::PartitionBound::Default) {
            if default.replace(child).is_some() {
                return Err(SQLError::Internal(format!(
                    "partitioned table `{parent}` has more than one default partition"
                )));
            }
            continue;
        }
        if partition_bound_matches(engine, bound, &keys, params)? {
            return Ok(Some(child));
        }
    }
    Ok(default)
}

fn evaluate_partition_keys(
    engine: &Engine,
    table: &str,
    expressions: &[uqa_sql::ast::Expr],
    document: &Document,
    params: &[SQLParam],
) -> Result<Vec<Value>, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read partition row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let schema = uqa_execution::RowSchema::with_types(
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect(),
        definitions
            .iter()
            .map(|definition| Some(definition.ty.clone()))
            .collect(),
    );
    expressions
        .iter()
        .map(|expression| {
            super::scalar::eval_lowered_expression_with_schema(
                engine, expression, document, &schema, params,
            )
        })
        .collect()
}

fn partition_bound_matches(
    engine: &Engine,
    bound: &uqa_sql::ast::PartitionBound,
    keys: &[Value],
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    use uqa_sql::ast::PartitionBound;
    match bound {
        PartitionBound::Default => Ok(true),
        PartitionBound::List(values) => {
            let [key] = keys else {
                return Err(SQLError::Internal(
                    "LIST partition has more than one partition key".into(),
                ));
            };
            for expression in values {
                if super::scalar::eval_lowered_expression(engine, expression, None, params)? == *key
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        PartitionBound::Range { lower, upper } => {
            if keys.iter().any(|value| matches!(value, Value::Null)) {
                return Ok(false);
            }
            Ok(
                compare_key_to_bound(engine, keys, lower, params)? != Ordering::Less
                    && compare_key_to_bound(engine, keys, upper, params)? == Ordering::Less,
            )
        }
        PartitionBound::Hash { .. } => Err(SQLError::Unsupported(
            "HASH partition routing is not implemented".into(),
        )),
    }
}

fn compare_key_to_bound(
    engine: &Engine,
    keys: &[Value],
    bound: &[uqa_sql::ast::PartitionRangeDatum],
    params: &[SQLParam],
) -> Result<Ordering, SQLError> {
    if keys.len() != bound.len() {
        return Err(SQLError::Internal(format!(
            "partition key width {} differs from bound width {}",
            keys.len(),
            bound.len()
        )));
    }
    for (key, datum) in keys.iter().zip(bound) {
        let ordering = match datum {
            uqa_sql::ast::PartitionRangeDatum::MinValue => Ordering::Greater,
            uqa_sql::ast::PartitionRangeDatum::MaxValue => Ordering::Less,
            uqa_sql::ast::PartitionRangeDatum::Value(expression) => key.cmp(
                &super::scalar::eval_lowered_expression(engine, expression, None, params)?,
            ),
        };
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
}

fn no_partition_for_row(table: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "23514".into(),
        message: format!("no partition of relation \"{table}\" found for row"),
    }
}
