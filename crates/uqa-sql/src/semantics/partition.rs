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
    /// The object identity of a table, which survives renames.
    fn try_table_object_id(&self, table: &str) -> Result<Option<[u8; 16]>, String>;
}

/// Evaluate declared partition keys and bounds with the caller's expression scope.
pub trait PartitionExpressions {
    /// Reconstruct a stored key using its retained type and routine identities and current catalog names.
    fn expression_text(&self, expression: &Expr) -> Result<String, SQLError>;
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
mod hash;
mod key;
pub use key::key_type as partition_key_type;

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
    Ok(RoutingKey { values, hash })
}

/// Why partition routing rejects a row. The executor reports each with a description of the row, which depends on the statement and the role that writes it.
#[derive(Debug, Clone, PartialEq)]
pub enum PartitionRejection {
    /// The row does not satisfy the partition constraint of `relation`, which a statement names and routes from: `new row for relation "x" violates partition constraint`.
    Constraint { relation: String },
    /// No partition of `relation` accepts the row, whose partition key has the values `keys`: `no partition of relation "x" found for row`.
    NoPartition { relation: String, keys: Vec<Value> },
}

/// Where a row that a statement writes through a relation is stored.
#[derive(Debug, Clone, PartialEq)]
pub enum PartitionRoute {
    /// The relation that stores the row.
    Target(String),
    /// Routing rejects the row.
    Rejected(PartitionRejection),
}

/// Route a row that an `INSERT` writes through `requested_table`, as `ExecFindPartition` does: a partitioned table that is itself a partition first checks its own partition constraint, and each level then selects the partition whose bound accepts the row. A relation that is not partitioned stores the row itself, without a check: `ExecInsert` checks the partition constraint of a partition that the statement names after its other constraints, which the caller does with [`partition_constraint_accepts_row`].
pub fn route_partition_insert(
    context: &PartitionContext<'_>,
    requested_table: &str,
    document: &ResultRow,
    params: &[SQLParam],
    include_descendants: bool,
) -> Result<PartitionRoute, SQLError> {
    let table = context
        .catalog
        .try_resolve_table_name(requested_table)
        .map_err(|error| SQLError::Internal(format!("resolve INSERT table: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(requested_table.to_string()))?;
    let hierarchy = context
        .catalog
        .try_table_hierarchy(&table)
        .map_err(|error| SQLError::Internal(format!("read partition metadata: {error}")))?;
    let Some(spec) = hierarchy.partition_spec.as_ref() else {
        return Ok(PartitionRoute::Target(table));
    };
    if !include_descendants {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("cannot insert into partitioned table \"{requested_table}\""),
        });
    }
    if !partition_constraint_accepts_row(context, &table, document, params)? {
        return Ok(PartitionRoute::Rejected(PartitionRejection::Constraint {
            relation: table,
        }));
    }
    route_partition_tree(context, &table, spec, document, params)
}

/// Whether the row satisfies the partition constraint of `table`, which holds the bounds of the partition and of each of its partition ancestors (`ExecPartitionCheck`). A relation that is not a partition accepts every row.
pub fn partition_constraint_accepts_row(
    context: &PartitionContext<'_>,
    table: &str,
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
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
            return Ok(true);
        }
        let parent = hierarchy.parents.first().ok_or_else(|| {
            SQLError::Internal(format!("partition `{child}` has no parent relation"))
        })?;
        let parent_hierarchy = context
            .catalog
            .try_table_hierarchy(parent)
            .map_err(|error| {
                SQLError::Internal(format!("read parent partition metadata: {error}"))
            })?;
        let spec = parent_hierarchy.partition_spec.as_ref().ok_or_else(|| {
            SQLError::Internal(format!("partition parent `{parent}` has no partition key"))
        })?;
        match select_direct_partition(context, parent, spec, document, params)? {
            DirectPartition::Child(selected) if selected == child => {}
            DirectPartition::Child(_) | DirectPartition::None { .. } => return Ok(false),
        }
        child.clone_from(parent);
    }
}

fn route_partition_tree(
    context: &PartitionContext<'_>,
    table: &str,
    spec: &crate::ast::PartitionSpec,
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<PartitionRoute, SQLError> {
    let child = match select_direct_partition(context, table, spec, document, params)? {
        DirectPartition::Child(child) => child,
        DirectPartition::None { keys } => {
            return Ok(PartitionRoute::Rejected(PartitionRejection::NoPartition {
                relation: table.to_string(),
                keys,
            }))
        }
    };
    let hierarchy = context
        .catalog
        .try_table_hierarchy(&child)
        .map_err(|error| SQLError::Internal(format!("read partition metadata: {error}")))?;
    match hierarchy.partition_spec.as_ref() {
        Some(spec) => route_partition_tree(context, &child, spec, document, params),
        None => Ok(PartitionRoute::Target(child)),
    }
}

/// The direct partition of a partitioned table that accepts a row, or the row's partition key values when none does.
enum DirectPartition {
    Child(String),
    None { keys: Vec<Value> },
}

fn select_direct_partition(
    context: &PartitionContext<'_>,
    parent: &str,
    spec: &crate::ast::PartitionSpec,
    document: &ResultRow,
    params: &[SQLParam],
) -> Result<DirectPartition, SQLError> {
    let key = routing_key(context, parent, spec, document, params)?;
    Ok(match select_partition(context, parent, &key, params)? {
        Some(child) => DirectPartition::Child(child),
        None => DirectPartition::None { keys: key.values },
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
mod order;
pub use order::partition_bound_order;
mod tree;
pub use identity::{
    foreign_key_scan_tables, partition_ancestor_tables, partition_hierarchy_root,
    partition_identity_owner,
};
pub use tree::{partition_tree, PartitionTreeNode};
