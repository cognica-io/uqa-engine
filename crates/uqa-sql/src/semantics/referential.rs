//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-key action discovery across partition and inheritance metadata.
use crate::{ast::ForeignKey, SQLError};

pub trait ReferentialCatalog {
    fn session_replication_role_is_replica(&self) -> bool;
    /// The relation followed by each partitioned table it is a partition of, nearest first, as [`crate::semantics::partition::partition_ancestor_tables`] returns them.
    fn partition_ancestor_tables(&self, table: &str) -> Result<Vec<String>, SQLError>;
    fn try_referrers_to(&self, table: &str) -> Result<Vec<(String, ForeignKey)>, String>;
}
use crate::catalog::errors::dml_storage_error;

pub fn referrers_to_for_actions(
    catalog: &dyn ReferentialCatalog,
    table: &str,
) -> Result<Vec<(String, ForeignKey)>, SQLError> {
    if catalog.session_replication_role_is_replica() {
        return Ok(Vec::new());
    }
    let mut output = Vec::new();
    // A row belongs to its partitions' ancestors, which foreign keys may reference; a foreign key referencing a plain inheritance parent reads only that parent's own rows.
    for target in catalog.partition_ancestor_tables(table)? {
        let referrers = catalog
            .try_referrers_to(&target)
            .map_err(|err| dml_storage_error("foreign-key lookup", err))?;
        for (declaring_table, foreign_key) in &referrers {
            if declares_foreign_key(catalog, &referrers, declaring_table, foreign_key)? {
                output.push((declaring_table.clone(), foreign_key.clone()));
            }
        }
    }
    Ok(output)
}

/// A foreign key declared on a partitioned table recurs on each of its partitions under the object identity of the declaration, and only the declaration acts on the rows of the whole subtree, as `PostgreSQL` creates action triggers for the constraint without a parent alone. A partition's own foreign key acts on that partition's rows only.
fn declares_foreign_key(
    catalog: &dyn ReferentialCatalog,
    referrers: &[(String, ForeignKey)],
    declaring_table: &str,
    foreign_key: &ForeignKey,
) -> Result<bool, SQLError> {
    let Some(object_id) = foreign_key.object_id else {
        return Ok(true);
    };
    for ancestor in catalog
        .partition_ancestor_tables(declaring_table)?
        .iter()
        .skip(1)
    {
        if referrers
            .iter()
            .any(|(table, key)| table == ancestor && key.object_id == Some(object_id))
        {
            return Ok(false);
        }
    }
    Ok(true)
}
