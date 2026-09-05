//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Partial-index predicates retain durable routine identities through DDL.

use super::{
    index_definition, Engine, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult,
};
use uqa_sql::ast::FunctionBinding;

impl Engine {
    pub(crate) fn indexes_depending_on_routine(
        &self,
        target: &FunctionBinding,
    ) -> Result<Vec<RelationIdentity>, SQLError> {
        let mut indexes = Vec::new();
        for row in self.durable.catalog_indexes.read().values() {
            let definition =
                index_definition(row).map_err(|error| SQLError::Internal(error.to_string()))?;
            if let Some(predicate) = definition.predicate.as_deref() {
                if crate::engine_events::expression_references_routine_identity(predicate, target)?
                {
                    indexes.push(row.relation.clone());
                }
            }
        }
        Ok(indexes)
    }

    pub(crate) fn rewrite_index_routine_identity(
        &self,
        target: &FunctionBinding,
        name: &str,
    ) -> StorageBackendResult<()> {
        let mut updates = Vec::new();
        for row in self.durable.catalog_indexes.read().values() {
            let mut definition = index_definition(row)?;
            let Some(predicate) = definition.predicate.as_deref_mut() else {
                continue;
            };
            if crate::engine_events::rewrite_expression_routine_identity(predicate, target, name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?
            {
                let mut row = row.clone();
                row.definition_json = Some(serde_json::to_string(&definition)?);
                updates.push(row);
            }
        }
        for row in updates {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog.save_catalog_index_row(&row)?;
            }
            self.durable
                .catalog_indexes
                .write()
                .insert(row.relation.clone(), row);
        }
        Ok(())
    }
}
