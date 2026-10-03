//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::PartitionCatalog;
use crate::SQLError;
use std::collections::BTreeSet;
pub fn partition_hierarchy_root(
    catalog: &dyn PartitionCatalog,
    table: &str,
) -> Result<Option<String>, SQLError> {
    let mut current = catalog
        .try_resolve_table_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut visited = BTreeSet::new();
    let mut participates = false;
    loop {
        if !visited.insert(current.clone()) {
            return Err(SQLError::Internal(format!(
                "partition hierarchy cycle reaches `{current}`"
            )));
        }
        let hierarchy = catalog
            .try_table_hierarchy(&current)
            .map_err(|error| SQLError::Internal(format!("read partition hierarchy: {error}")))?;
        participates |= hierarchy.partition_spec.is_some() || hierarchy.is_partition();
        if !hierarchy.is_partition() {
            return Ok(participates.then_some(current));
        }
        current = hierarchy
            .parents
            .first()
            .cloned()
            .ok_or_else(|| SQLError::Internal("partition has no parent relation".into()))?;
    }
}

fn resolve_table(catalog: &dyn PartitionCatalog, table: &str) -> Result<String, SQLError> {
    catalog
        .try_resolve_table_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))
}

/// The relation followed by each partitioned table it is a partition of, nearest first. A foreign key that references a table which is not partitioned reads `ONLY` that table, as `PostgreSQL`'s referential queries do, so plain inheritance parents are not among them.
pub fn partition_ancestor_tables(
    catalog: &dyn PartitionCatalog,
    table: &str,
) -> Result<Vec<String>, SQLError> {
    let mut current = resolve_table(catalog, table)?;
    let mut output = Vec::new();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return Err(SQLError::Internal(format!(
                "partition hierarchy cycle reaches `{current}`"
            )));
        }
        let hierarchy = catalog
            .try_table_hierarchy(&current)
            .map_err(|error| SQLError::Internal(format!("read partition hierarchy: {error}")))?;
        output.push(current);
        if !hierarchy.is_partition() {
            return Ok(output);
        }
        current = hierarchy
            .parents
            .first()
            .cloned()
            .ok_or_else(|| SQLError::Internal("partition has no parent relation".into()))?;
    }
}

/// The relations whose rows a foreign key reads for `table`: the table and, when it is partitioned, every partition below it, each before its own partitions. The rows of a plain inheritance child belong to the child alone, as `PostgreSQL`'s referential queries read `ONLY` a table that is not partitioned.
pub fn foreign_key_scan_tables(
    catalog: &dyn PartitionCatalog,
    table: &str,
) -> Result<Vec<String>, SQLError> {
    let mut pending = vec![resolve_table(catalog, table)?];
    let mut output = Vec::new();
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop() {
        if !visited.insert(current.clone()) {
            return Err(SQLError::Internal(format!(
                "partition hierarchy cycle reaches `{current}`"
            )));
        }
        let hierarchy = catalog
            .try_table_hierarchy(&current)
            .map_err(|error| SQLError::Internal(format!("read partition hierarchy: {error}")))?;
        if hierarchy.partition_spec.is_some() {
            let mut partitions = catalog.direct_hierarchy_children(&current)?;
            partitions.reverse();
            pending.extend(partitions);
        }
        output.push(current);
    }
    Ok(output)
}

/// Return the physical counter owner for legacy auto-increment metadata. Declarative partitions share the top partitioned parent's counter; newly created SERIAL and identity columns use their durable sequence binding instead.
pub fn partition_identity_owner(
    catalog: &dyn PartitionCatalog,
    table: &str,
) -> Result<String, SQLError> {
    if let Some(root) = partition_hierarchy_root(catalog, table)? {
        return Ok(root);
    }
    catalog
        .try_resolve_table_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))
}

#[cfg(test)]
mod tests;
