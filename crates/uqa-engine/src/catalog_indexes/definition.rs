//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored index semantics and the common key-enforcement boundary.

use crate::capabilities::RelationLookupMode;
use crate::{Engine, StorageBackendError, StorageBackendResult};
use uqa_sql::ast::TableKeyConstraint;

pub(crate) use uqa_sql::catalog::index::IndexDefinition;

pub(crate) use uqa_execution::catalog::index::index_definition;

impl Engine {
    /// Key descriptors used by row validation, key reservations, and conflict arbitration. Standalone unique indexes remain independent catalog objects and do not create SQL constraints.
    pub(crate) fn enforced_keys(&self, table: &str) -> StorageBackendResult<Vec<EnforcedKey>> {
        let constraints = self.key_constraints_in_execution(table)?;
        let table = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        // The index rows the catalog view of this statement holds.
        let indexes = match self.query_catalog_snapshot.as_ref() {
            Some(snapshot) => std::sync::Arc::clone(&snapshot.catalog_indexes),
            None => self.durable.catalog_indexes.snapshot(),
        };
        self.runtime
            .enforced_key_cache
            .keys(&table, constraints, &indexes, |constraints| {
                let catalog = self.catalog_read_view();
                let mut resolution = self.session_execution_view().relation_name_resolution();
                resolution.set_lookup_mode(RelationLookupMode::Bound);
                uqa_execution::catalog::index::enforced_keys(
                    &catalog,
                    &resolution,
                    &table,
                    constraints,
                )
            })
    }
}

pub(crate) use uqa_sql::catalog::index::EnforcedKey;

impl Engine {
    pub(crate) fn referenceable_keys(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<TableKeyConstraint>> {
        Ok(uqa_sql::catalog::index::referenceable_keys(
            self.enforced_keys(table)?,
        ))
    }
}
