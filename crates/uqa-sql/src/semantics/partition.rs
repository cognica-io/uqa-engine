//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declarative partition validation, value comparison, and routing semantics.

use crate::{
    ast::{ColumnDef, Expr, TableHierarchy},
    type_resolution::FunctionTypeResolver,
    ResultRow, RowSchema, SQLError, SQLParam,
};
use std::cmp::Ordering;
use uqa_core::Value;

/// Hierarchy definitions needed to validate and route a row; no storage writes are exposed.
pub trait PartitionCatalog {
    fn try_table_hierarchy(&self, table: &str) -> Result<TableHierarchy, String>;
    fn direct_hierarchy_children(&self, parent: &str) -> Result<Vec<String>, SQLError>;
    fn try_resolve_table_name(&self, name: &str) -> Result<Option<String>, String>;
    fn try_describe_table(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String>;
}

/// Evaluate declared partition keys and bounds with the caller's expression scope.
pub trait PartitionExpressions {
    fn evaluate_bound(&self, expression: &Expr, params: &[SQLParam]) -> Result<Value, SQLError>;
    fn evaluate_row(
        &self,
        expression: &Expr,
        row: &ResultRow,
        schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Value, SQLError>;
}

#[derive(Clone, Copy)]
pub struct PartitionContext<'a> {
    pub catalog: &'a dyn PartitionCatalog,
    pub expressions: &'a dyn PartitionExpressions,
    pub types: &'a dyn FunctionTypeResolver,
}

mod hash;

pub fn validate_hash_partition_spec(
    context: &PartitionContext<'_>,
    spec: &crate::ast::PartitionSpec,
    columns: &[crate::ast::ColumnDef],
) -> Result<(), SQLError> {
    hash::validate_partition_spec(context.types, spec, columns)
}

pub fn validate_new_partition_bound(
    context: &PartitionContext<'_>,
    parent: &str,
    bound: &crate::ast::PartitionBound,
) -> Result<(), SQLError> {
    let hierarchy = context
        .catalog
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
    if let crate::ast::PartitionBound::Hash { modulus, remainder } = bound {
        hash::validate_bound(*modulus, *remainder)?;
        let mut existing_moduli = Vec::new();
        for sibling in context.catalog.direct_hierarchy_children(parent)? {
            let sibling_hierarchy = context
                .catalog
                .try_table_hierarchy(&sibling)
                .map_err(|error| SQLError::Internal(format!("read sibling partition: {error}")))?;
            match sibling_hierarchy.partition_bound.as_ref() {
                Some(crate::ast::PartitionBound::Hash { modulus, remainder }) => {
                    hash::validate_bound(*modulus, *remainder)?;
                    existing_moduli.push(*modulus);
                }
                Some(crate::ast::PartitionBound::Default) => {
                    return Err(SQLError::Internal(format!(
                        "HASH-partitioned table `{parent}` has a default partition"
                    )))
                }
                Some(_) => {
                    return Err(SQLError::Internal(
                        "partition siblings use different bound strategies".into(),
                    ))
                }
                None => {}
            }
        }
        hash::validate_modulus_chain(*modulus, existing_moduli)?;
    }
    if let crate::ast::PartitionBound::Range { lower, upper } = bound {
        if compare_partition_points(context, lower, upper)? != Ordering::Less {
            return Err(invalid_partition_bound(
                "empty range bound specified for partition",
            ));
        }
    }
    for sibling in context.catalog.direct_hierarchy_children(parent)? {
        let sibling_hierarchy = context
            .catalog
            .try_table_hierarchy(&sibling)
            .map_err(|error| SQLError::Internal(format!("read sibling partition: {error}")))?;
        let Some(sibling_bound) = sibling_hierarchy.partition_bound.as_ref() else {
            continue;
        };
        if partition_bounds_overlap(context, bound, sibling_bound)? {
            return Err(invalid_partition_bound(format!(
                "partition would overlap partition \"{sibling}\""
            )));
        }
    }
    Ok(())
}

/// Test one stored row against a prospective direct-child bound before the
/// hierarchy edge is installed. DEFAULT means no existing non-default sibling
/// accepts the row, matching the routing decision the new edge will expose.
pub fn prospective_partition_bound_accepts_document(
    context: &PartitionContext<'_>,
    parent: &str,
    bound: &crate::ast::PartitionBound,
    document: &ResultRow,
) -> Result<bool, SQLError> {
    let hierarchy = context
        .catalog
        .try_table_hierarchy(parent)
        .map_err(|error| SQLError::Internal(format!("read parent partition metadata: {error}")))?;
    let spec = hierarchy
        .partition_spec
        .as_ref()
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("relation \"{parent}\" is not partitioned"),
        })?;
    let (keys, row_hash) = partition_key_values_and_hash(context, parent, spec, document)?;
    if !matches!(bound, crate::ast::PartitionBound::Default) {
        return partition_bound_matches(context, bound, &keys, &[], row_hash);
    }
    for sibling in context.catalog.direct_hierarchy_children(parent)? {
        let sibling_hierarchy = context
            .catalog
            .try_table_hierarchy(&sibling)
            .map_err(|error| SQLError::Internal(format!("read child partition: {error}")))?;
        let Some(sibling_bound) = sibling_hierarchy.partition_bound.as_ref() else {
            continue;
        };
        if matches!(sibling_bound, crate::ast::PartitionBound::Default) {
            continue;
        }
        if partition_bound_matches(context, sibling_bound, &keys, &[], row_hash)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Evaluate a retained detached-partition CHECK without requiring a live
/// parent edge. This is also the exact predicate used for prospective ATTACH
/// row scans.
pub fn partition_constraint_accepts_document(
    context: &PartitionContext<'_>,
    table: &str,
    spec: &crate::ast::PartitionSpec,
    bound: &crate::ast::PartitionBound,
    document: &ResultRow,
) -> Result<bool, SQLError> {
    let (keys, row_hash) = partition_key_values_and_hash(context, table, spec, document)?;
    partition_bound_matches(context, bound, &keys, &[], row_hash)
}

fn partition_key_values_and_hash(
    context: &PartitionContext<'_>,
    table: &str,
    spec: &crate::ast::PartitionSpec,
    document: &ResultRow,
) -> Result<(Vec<Value>, Option<u64>), SQLError> {
    let (keys, definitions) = evaluate_partition_keys(context, table, &spec.keys, document, &[])?;
    let row_hash = (spec.strategy == crate::ast::PartitionStrategy::Hash)
        .then(|| hash::row_hash(context.types, spec, &definitions, &keys))
        .transpose()?;
    Ok((keys, row_hash))
}

fn validate_partition_bound_width(
    spec: &crate::ast::PartitionSpec,
    bound: &crate::ast::PartitionBound,
) -> Result<(), SQLError> {
    use crate::ast::{PartitionBound, PartitionStrategy};
    match (spec.strategy, bound) {
        (_, PartitionBound::Default) => Ok(()),
        (PartitionStrategy::List, PartitionBound::List(_)) if spec.keys.len() != 1 => {
            Err(invalid_partition_bound(
                "cannot use list partition bounds with more than one partition key",
            ))
        }
        (PartitionStrategy::List, PartitionBound::List(_)) => Ok(()),
        (PartitionStrategy::Range, PartitionBound::Range { lower, upper })
            if lower.len() != spec.keys.len() || upper.len() != spec.keys.len() =>
        {
            Err(invalid_partition_bound(
                "partition bound has the wrong number of columns",
            ))
        }
        (PartitionStrategy::Range, PartitionBound::Range { .. })
        | (PartitionStrategy::Hash, PartitionBound::Hash { .. }) => Ok(()),
        (strategy, _) => Err(invalid_partition_bound(format!(
            "invalid bound specification for a {} partitioned table",
            match strategy {
                PartitionStrategy::List => "list",
                PartitionStrategy::Range => "range",
                PartitionStrategy::Hash => "hash",
            }
        ))),
    }
}

fn partition_bounds_overlap(
    context: &PartitionContext<'_>,
    left: &crate::ast::PartitionBound,
    right: &crate::ast::PartitionBound,
) -> Result<bool, SQLError> {
    use crate::ast::PartitionBound;
    match (left, right) {
        (PartitionBound::Default, PartitionBound::Default) => Ok(true),
        (PartitionBound::Default, _) | (_, PartitionBound::Default) => Ok(false),
        (PartitionBound::List(left), PartitionBound::List(right)) => {
            let left = evaluate_bound_values(context, left)?;
            let right = evaluate_bound_values(context, right)?;
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
            compare_partition_points(context, left_lower, right_upper)? == Ordering::Less
                && compare_partition_points(context, right_lower, left_upper)? == Ordering::Less,
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
        ) => hash::bounds_overlap(
            *left_modulus,
            *left_remainder,
            *right_modulus,
            *right_remainder,
        ),
        _ => Err(SQLError::Internal(
            "partition siblings use different bound strategies".into(),
        )),
    }
}

fn evaluate_bound_values(
    context: &PartitionContext<'_>,
    expressions: &[crate::ast::Expr],
) -> Result<Vec<Value>, SQLError> {
    expressions
        .iter()
        .map(|expression| context.expressions.evaluate_bound(expression, &[]))
        .collect()
}

fn compare_partition_points(
    context: &PartitionContext<'_>,
    left: &[crate::ast::PartitionRangeDatum],
    right: &[crate::ast::PartitionRangeDatum],
) -> Result<Ordering, SQLError> {
    if left.len() != right.len() {
        return Err(invalid_partition_bound(
            "partition range points have different widths",
        ));
    }
    for (left, right) in left.iter().zip(right) {
        let ordering = match (left, right) {
            (
                crate::ast::PartitionRangeDatum::MinValue,
                crate::ast::PartitionRangeDatum::MinValue,
            )
            | (
                crate::ast::PartitionRangeDatum::MaxValue,
                crate::ast::PartitionRangeDatum::MaxValue,
            ) => Ordering::Equal,
            (crate::ast::PartitionRangeDatum::MinValue, _)
            | (_, crate::ast::PartitionRangeDatum::MaxValue) => Ordering::Less,
            (crate::ast::PartitionRangeDatum::MaxValue, _)
            | (_, crate::ast::PartitionRangeDatum::MinValue) => Ordering::Greater,
            (
                crate::ast::PartitionRangeDatum::Value(left),
                crate::ast::PartitionRangeDatum::Value(right),
            ) => context
                .expressions
                .evaluate_bound(left, &[])?
                .cmp(&context.expressions.evaluate_bound(right, &[])?),
        };
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
}

fn invalid_partition_bound(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P17".into(),
        message: message.into(),
    }
}

pub fn partition_insert_target(
    context: &PartitionContext<'_>,
    requested_table: &str,
    document: &ResultRow,
    params: &[SQLParam],
    include_descendants: bool,
) -> Result<String, SQLError> {
    let table = context
        .catalog
        .try_resolve_table_name(requested_table)
        .map_err(|error| SQLError::Internal(format!("resolve INSERT table: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(requested_table.to_string()))?;
    let hierarchy = context
        .catalog
        .try_table_hierarchy(&table)
        .map_err(|error| SQLError::Internal(format!("read partition metadata: {error}")))?;
    validate_partition_ancestor_path(context, requested_table, &table, document, params)?;
    if let Some(spec) = hierarchy.partition_spec.as_ref() {
        if !include_descendants {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot insert into partitioned table \"{requested_table}\""),
            });
        }
        let child = select_direct_partition_with_spec(context, &table, spec, document, params)?
            .ok_or_else(|| no_partition_for_row(requested_table))?;
        return route_partition_tree(context, &child, document, params);
    }
    Ok(table)
}

fn validate_partition_ancestor_path(
    context: &PartitionContext<'_>,
    requested_table: &str,
    table: &str,
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let mut child = table.to_string();
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(child.clone()) {
            return Err(SQLError::Internal(format!(
                "partition hierarchy cycle reaches `{child}`"
            )));
        }
        let hierarchy = context
            .catalog
            .try_table_hierarchy(&child)
            .map_err(|error| SQLError::Internal(format!("read partition metadata: {error}")))?;
        if hierarchy.partition_bound.is_none() {
            return Ok(());
        }
        let parent = hierarchy.parents.first().ok_or_else(|| {
            SQLError::Internal(format!("partition `{child}` has no parent relation"))
        })?;
        let selected = select_direct_partition(context, parent, document, params)?;
        if selected.as_deref() != Some(child.as_str()) {
            return Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: format!(
                    "new row for relation \"{requested_table}\" violates partition constraint"
                ),
            });
        }
        child.clone_from(parent);
    }
}

fn route_partition_tree(
    context: &PartitionContext<'_>,
    table: &str,
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<String, SQLError> {
    let hierarchy = context
        .catalog
        .try_table_hierarchy(table)
        .map_err(|error| SQLError::Internal(format!("read partition metadata: {error}")))?;
    let Some(spec) = hierarchy.partition_spec.as_ref() else {
        return Ok(table.to_string());
    };
    let child = select_direct_partition_with_spec(context, table, spec, document, params)?
        .ok_or_else(|| no_partition_for_row(table))?;
    route_partition_tree(context, &child, document, params)
}

fn select_direct_partition(
    context: &PartitionContext<'_>,
    parent: &str,
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<Option<String>, SQLError> {
    let hierarchy = context
        .catalog
        .try_table_hierarchy(parent)
        .map_err(|error| SQLError::Internal(format!("read parent partition metadata: {error}")))?;
    let spec = hierarchy.partition_spec.as_ref().ok_or_else(|| {
        SQLError::Internal(format!("partition parent `{parent}` has no partition key"))
    })?;
    select_direct_partition_with_spec(context, parent, spec, document, params)
}

fn select_direct_partition_with_spec(
    context: &PartitionContext<'_>,
    parent: &str,
    spec: &crate::ast::PartitionSpec,
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<Option<String>, SQLError> {
    let (keys, definitions) =
        evaluate_partition_keys(context, parent, &spec.keys, document, params)?;
    let row_hash = (spec.strategy == crate::ast::PartitionStrategy::Hash)
        .then(|| hash::row_hash(context.types, spec, &definitions, &keys))
        .transpose()?;
    let mut default = None;
    for child in context.catalog.direct_hierarchy_children(parent)? {
        let child_hierarchy = context
            .catalog
            .try_table_hierarchy(&child)
            .map_err(|error| SQLError::Internal(format!("read child partition: {error}")))?;
        let Some(bound) = child_hierarchy.partition_bound.as_ref() else {
            continue;
        };
        if matches!(bound, crate::ast::PartitionBound::Default) {
            if default.replace(child).is_some() {
                return Err(SQLError::Internal(format!(
                    "partitioned table `{parent}` has more than one default partition"
                )));
            }
            continue;
        }
        if partition_bound_matches(context, bound, &keys, params, row_hash)? {
            return Ok(Some(child));
        }
    }
    Ok(default)
}

fn evaluate_partition_keys(
    context: &PartitionContext<'_>,
    table: &str,
    expressions: &[crate::ast::Expr],
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<(Vec<Value>, Vec<crate::ast::ColumnDef>), SQLError> {
    let definitions = context
        .catalog
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read partition row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let schema = crate::RowSchema::with_types(
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect(),
        definitions
            .iter()
            .map(|definition| Some(definition.ty.clone()))
            .collect(),
    );
    let values = expressions
        .iter()
        .map(|expression| {
            context
                .expressions
                .evaluate_row(expression, document, &schema, params)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((values, definitions))
}

fn partition_bound_matches(
    context: &PartitionContext<'_>,
    bound: &crate::ast::PartitionBound,
    keys: &[Value],
    params: &[SQLParam],
    row_hash: Option<u64>,
) -> Result<bool, SQLError> {
    use crate::ast::PartitionBound;
    match bound {
        PartitionBound::Default => Ok(true),
        PartitionBound::List(values) => {
            let [key] = keys else {
                return Err(SQLError::Internal(
                    "LIST partition has more than one partition key".into(),
                ));
            };
            for expression in values {
                if context.expressions.evaluate_bound(expression, params)? == *key {
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
                compare_key_to_bound(context, keys, lower, params)? != Ordering::Less
                    && compare_key_to_bound(context, keys, upper, params)? == Ordering::Less,
            )
        }
        PartitionBound::Hash { modulus, remainder } => hash::bound_matches(
            row_hash.ok_or_else(|| {
                SQLError::Internal("HASH partition bound has no computed row hash".into())
            })?,
            *modulus,
            *remainder,
        ),
    }
}

fn compare_key_to_bound(
    context: &PartitionContext<'_>,
    keys: &[Value],
    bound: &[crate::ast::PartitionRangeDatum],
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
            crate::ast::PartitionRangeDatum::MinValue => Ordering::Greater,
            crate::ast::PartitionRangeDatum::MaxValue => Ordering::Less,
            crate::ast::PartitionRangeDatum::Value(expression) => {
                key.cmp(&context.expressions.evaluate_bound(expression, params)?)
            }
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

mod identity;
pub use identity::{partition_hierarchy_root, partition_identity_owner};
