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
