//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-key action discovery across partition and inheritance metadata.
use crate::{ast::ForeignKey, SQLError};

pub trait ReferentialCatalog {
    fn session_replication_role_is_replica(&self) -> bool;
    fn hierarchy_ancestor_tables(&self, table: &str) -> Result<Vec<String>, SQLError>;
    fn try_referrers_to(&self, table: &str) -> Result<Vec<(String, ForeignKey)>, String>;
    fn partition_hierarchy_root(&self, table: &str) -> Result<Option<String>, SQLError>;
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
    for target in catalog.hierarchy_ancestor_tables(table)? {
        let referrers = catalog
            .try_referrers_to(&target)
            .map_err(|err| dml_storage_error("foreign-key lookup", err))?;
        for (declaring_table, foreign_key) in referrers {
            let referencing_table = catalog
                .partition_hierarchy_root(&declaring_table)?
                .unwrap_or(declaring_table);
            if output.iter().any(|(existing_table, existing_key)| {
                existing_table == &referencing_table
                    && foreign_keys_equivalent(existing_key, &foreign_key)
            }) {
                continue;
            }
            output.push((referencing_table, foreign_key));
        }
    }
    Ok(output)
}

fn foreign_keys_equivalent(left: &ForeignKey, right: &ForeignKey) -> bool {
    left.name == right.name
        && left.local_columns == right.local_columns
        && left.ref_table == right.ref_table
        && left.ref_columns == right.ref_columns
        && left.on_update == right.on_update
        && left.on_delete == right.on_delete
        && left.on_delete_set_columns == right.on_delete_set_columns
        && left.match_type == right.match_type
        && left.enforced == right.enforced
}
