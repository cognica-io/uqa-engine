//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declarative partition validation, value comparison, and routing semantics.

use crate::{
    assignment::AssignmentContext,
    ast::{ColumnDef, Expr, TableHierarchy},
    schema::SchemaExpressionCatalog,
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
    /// Whether the current role may see the partition key of `table` in a diagnostic: SELECT on the table, or on every key column when no key is an expression (`None`).
    fn can_view_partition_key(
        &self,
        table: &str,
        columns: &[Option<&str>],
    ) -> Result<bool, SQLError>;
    /// `Failing row contains ...` for a row of `table` as the current role may see it.
    fn failing_row_detail(&self, table: &str, row: &ResultRow) -> Result<Option<String>, SQLError>;
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
    /// Catalog-aware assignment coercion of bound values and output of key values.
    pub assignment: &'a dyn AssignmentContext,
    /// Aggregate, window and set-returning classification of bound and key expressions.
    pub schema: &'a dyn SchemaExpressionCatalog,
}

mod admission;
mod bounds;
mod datum_text;
mod description;
mod hash;
mod key;

pub use admission::validate_new_partition_bound;
pub use bounds::transform_partition_bound;
pub use datum_text::{partition_datum_text, range_bound_text, stored_datum};

pub fn validate_hash_partition_spec(
    context: &PartitionContext<'_>,
    spec: &crate::ast::PartitionSpec,
    columns: &[crate::ast::ColumnDef],
) -> Result<(), SQLError> {
    hash::validate_partition_spec(context.types, spec, columns)
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
    let key = routing_key(context, parent, spec, document, &[])?;
    if !matches!(bound, crate::ast::PartitionBound::Default) {
        return partition_bound_matches(context, bound, &key, &[]);
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
        if partition_bound_matches(context, sibling_bound, &key, &[])? {
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
    let key = routing_key(context, table, spec, document, &[])?;
    partition_bound_matches(context, bound, &key, &[])
}

/// A row's partition key under one partitioned table, with the row hash of a HASH partition key.
struct RoutingKey {
    values: Vec<Value>,
    definitions: Vec<ColumnDef>,
    hash: Option<u64>,
}

fn routing_key(
    context: &PartitionContext<'_>,
    table: &str,
    spec: &crate::ast::PartitionSpec,
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<RoutingKey, SQLError> {
    let (values, definitions) =
        evaluate_partition_keys(context, table, &spec.keys, document, params)?;
    let hash = (spec.strategy == crate::ast::PartitionStrategy::Hash)
        .then(|| hash::row_hash(context, spec, &definitions, &values))
        .transpose()?;
    Ok(RoutingKey {
        values,
        definitions,
        hash,
    })
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
    validate_partition_ancestor_path(context, &table, document, params)?;
    if let Some(spec) = hierarchy.partition_spec.as_ref() {
        if !include_descendants {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("cannot insert into partitioned table \"{requested_table}\""),
            });
        }
        let child = route_direct_partition(context, &table, spec, document, params)?;
        return route_partition_tree(context, &child, document, params);
    }
    Ok(table)
}

fn validate_partition_ancestor_path(
    context: &PartitionContext<'_>,
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
            return Err(SQLError::Diagnostic {
                sqlstate: "23514".into(),
                message: format!(
                    "new row for relation \"{}\" violates partition constraint",
                    admission::local_relation_name(table)?
                ),
                detail: context.catalog.failing_row_detail(table, document)?,
                hint: None,
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
    let child = route_direct_partition(context, table, spec, document, params)?;
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
    let key = routing_key(context, parent, spec, document, params)?;
    select_partition(context, parent, &key, params)
}

/// Route one level down, reporting `PostgreSQL`'s routing failure with the row's partition key.
fn route_direct_partition(
    context: &PartitionContext<'_>,
    table: &str,
    spec: &crate::ast::PartitionSpec,
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<String, SQLError> {
    let key = routing_key(context, table, spec, document, params)?;
    if let Some(child) = select_partition(context, table, &key, params)? {
        return Ok(child);
    }
    let columns = key::key_columns(context.types, spec, &key.definitions)?;
    Err(SQLError::Diagnostic {
        sqlstate: "23514".into(),
        message: format!(
            "no partition of relation \"{}\" found for row",
            admission::local_relation_name(table)?
        ),
        detail: description::partition_key_detail(context, table, &columns, &key.values)?,
        hint: None,
    })
}

fn select_partition(
    context: &PartitionContext<'_>,
    parent: &str,
    key: &RoutingKey,
    params: &[SQLParam],
) -> Result<Option<String>, SQLError> {
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
        if partition_bound_matches(context, bound, key, params)? {
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
    key: &RoutingKey,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    use crate::ast::PartitionBound;
    let keys = key.values.as_slice();
    match bound {
        PartitionBound::Default => Ok(true),
        PartitionBound::List(values) => {
            let [key] = keys else {
                return Err(SQLError::Internal(
                    "LIST partition has more than one partition key".into(),
                ));
            };
            for expression in values {
                if context
                    .expressions
                    .evaluate_bound(expression, params)?
                    .cmp(key)
                    == Ordering::Equal
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
                compare_key_to_bound(context, keys, lower, params)? != Ordering::Less
                    && compare_key_to_bound(context, keys, upper, params)? == Ordering::Less,
            )
        }
        PartitionBound::Hash { modulus, remainder } => hash::bound_matches(
            key.hash.ok_or_else(|| {
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

mod identity;
pub use identity::{partition_hierarchy_root, partition_identity_owner};
