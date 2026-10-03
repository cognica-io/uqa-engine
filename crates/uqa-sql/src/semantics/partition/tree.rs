//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The partitions of a partitioned table in the order `PostgreSQL` visits them when it derives objects on them.

use std::collections::BTreeSet;

use super::{partition_bound_order, PartitionContext};
use crate::SQLError;

/// A partition and the partitioned table it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionTreeNode {
    pub table: String,
    pub object_id: [u8; 16],
    pub parent: String,
    pub parent_object_id: [u8; 16],
}

fn object_id(context: &PartitionContext<'_>, table: &str) -> Result<[u8; 16], SQLError> {
    context
        .catalog
        .try_table_object_id(table)
        .map_err(|error| SQLError::Internal(format!("read table identity: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))
}

/// The partitions below `root`, and `root` itself first when `include_root` names a partition, each before its own partitions, with siblings in partition order.
pub fn partition_tree(
    context: &PartitionContext<'_>,
    root: &str,
    include_root: bool,
) -> Result<Vec<PartitionTreeNode>, SQLError> {
    let mut output = Vec::new();
    let mut visited = BTreeSet::new();
    let parent = if include_root {
        let hierarchy = context
            .catalog
            .try_table_hierarchy(root)
            .map_err(|error| SQLError::Internal(format!("read partition hierarchy: {error}")))?;
        let parent = hierarchy
            .parents
            .first()
            .filter(|_| hierarchy.is_partition())
            .cloned()
            .ok_or_else(|| SQLError::Internal(format!("`{root}` is not a partition")))?;
        let parent_object_id = object_id(context, &parent)?;
        Some((parent, parent_object_id))
    } else {
        None
    };
    visit(context, root, parent, &mut visited, &mut output)?;
    Ok(output)
}

fn visit(
    context: &PartitionContext<'_>,
    table: &str,
    parent: Option<(String, [u8; 16])>,
    visited: &mut BTreeSet<String>,
    output: &mut Vec<PartitionTreeNode>,
) -> Result<(), SQLError> {
    if !visited.insert(table.to_string()) {
        return Err(SQLError::Internal(format!(
            "partition hierarchy cycle reaches `{table}`"
        )));
    }
    let table_object_id = object_id(context, table)?;
    if let Some((parent, parent_object_id)) = parent {
        output.push(PartitionTreeNode {
            table: table.to_string(),
            object_id: table_object_id,
            parent,
            parent_object_id,
        });
    }
    let hierarchy = context
        .catalog
        .try_table_hierarchy(table)
        .map_err(|error| SQLError::Internal(format!("read partition hierarchy: {error}")))?;
    if hierarchy.partition_spec.is_none() {
        return Ok(());
    }
    let mut partitions = Vec::new();
    for child in context.catalog.direct_hierarchy_children(table)? {
        let bound = context
            .catalog
            .try_table_hierarchy(&child)
            .map_err(|error| SQLError::Internal(format!("read partition hierarchy: {error}")))?
            .partition_bound
            .ok_or_else(|| {
                SQLError::Internal(format!("partition `{child}` of `{table}` has no bound"))
            })?;
        partitions.push((child, bound));
    }
    for child in partition_bound_order(context, partitions)? {
        visit(
            context,
            &child,
            Some((table.to_string(), table_object_id)),
            visited,
            output,
        )?;
    }
    Ok(())
}
