//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint-owned index identities in catalog snapshots.

use super::{CatalogReadView, SQLError};

impl CatalogReadView {
    pub(crate) fn has_constraint_index(&self, relation: &crate::RelationIdentity) -> bool {
        self.snapshot.tables.iter().any(|(table, snapshot)| {
            table.schema == relation.schema
                && snapshot
                    .keys
                    .iter()
                    .any(|key| key.name.as_ref() == Some(&relation.name))
        })
    }

    pub(crate) fn constraint_index(
        &self,
        relation: &crate::RelationIdentity,
    ) -> Result<Option<uqa_storage::CatalogIndexRow>, SQLError> {
        for (table, snapshot) in &self.snapshot.tables {
            if table.schema != relation.schema {
                continue;
            }
            let Some(key) = snapshot
                .keys
                .iter()
                .find(|key| key.name.as_ref() == Some(&relation.name))
            else {
                continue;
            };
            let definition = crate::engine_catalog_indexes::IndexDefinition {
                unique: true,
                nulls_not_distinct: key.nulls_not_distinct,
                ..Default::default()
            };
            return Ok(Some(uqa_storage::CatalogIndexRow {
                relation: relation.clone(),
                table_name: table.qualified_name(),
                index_type: if key.without_overlaps {
                    "gist"
                } else {
                    "btree"
                }
                .into(),
                columns_json: serde_json::to_string(&key.columns)
                    .map_err(|error| SQLError::Internal(error.to_string()))?,
                parameters_json: "{}".into(),
                definition_json: Some(
                    serde_json::to_string(&definition)
                        .map_err(|error| SQLError::Internal(error.to_string()))?,
                ),
            }));
        }
        Ok(None)
    }
}
