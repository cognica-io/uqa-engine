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
            let keys: Vec<uqa_sql::ast::IndexKey> = serde_json::from_str(&row.columns_json)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            for expression in keys
                .iter()
                .filter_map(|key| match key {
                    uqa_sql::ast::IndexKey::Expression(expr) => Some(expr.as_ref()),
                    uqa_sql::ast::IndexKey::Column(_) => None,
                })
                .chain(definition.predicate.as_deref())
            {
                if crate::engine_events::expression_references_routine_identity(expression, target)?
                {
                    indexes.push(row.relation.clone());
                    break;
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
            let mut keys: Vec<uqa_sql::ast::IndexKey> = serde_json::from_str(&row.columns_json)?;
            let mut changed = false;
            for expression in keys
                .iter_mut()
                .filter_map(|key| match key {
                    uqa_sql::ast::IndexKey::Expression(expr) => Some(expr.as_mut()),
                    uqa_sql::ast::IndexKey::Column(_) => None,
                })
                .chain(definition.predicate.as_deref_mut())
            {
                changed |= crate::engine_events::rewrite_expression_routine_identity(
                    expression, target, name,
                )
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            }
            if changed {
                let mut row = row.clone();
                row.columns_json = serde_json::to_string(&keys)?;
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
