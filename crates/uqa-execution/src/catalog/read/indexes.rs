//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint-owned index identities in catalog snapshots.

use super::{CatalogReadView, SQLError};

impl CatalogReadView {
    pub fn has_constraint_index(&self, relation: &uqa_core::RelationIdentity) -> bool {
        self.snapshot
            .definitions
            .catalog_indexes
            .get(relation)
            .is_some_and(|row| {
                crate::catalog::index::index_definition(row)
                    .is_ok_and(|definition| definition.relationships.owning_constraint.is_some())
            })
    }

    pub fn constraint_index(
        &self,
        relation: &uqa_core::RelationIdentity,
    ) -> Result<Option<uqa_storage::CatalogIndexRow>, SQLError> {
        let Some(row) = self.snapshot.definitions.catalog_indexes.get(relation) else {
            return Ok(None);
        };
        let definition = crate::catalog::index::index_definition(row)
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        Ok(definition
            .relationships
            .owning_constraint
            .map(|_| row.clone()))
    }
}
