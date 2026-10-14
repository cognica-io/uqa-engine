//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared relation binding over names and immutable definition registries.

use super::{
    CatalogDefinitionSnapshot, RelationLookupMode, RelationNameResolution, RelationResolution,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

/// Table membership is sufficient to bind a relation; callers need not load table definitions.
pub trait RelationTableNames {
    fn contains_table(&self, relation: &RelationIdentity) -> bool;
}

impl<T> RelationTableNames for BTreeMap<RelationIdentity, T> {
    fn contains_table(&self, relation: &RelationIdentity) -> bool {
        self.contains_key(relation)
    }
}

/// The shared SQL namespace, borrowing table names without copying their metadata.
pub struct RelationNameCatalog<'a> {
    tables: &'a dyn RelationTableNames,
    definitions: &'a CatalogDefinitionSnapshot,
}

impl<'a> RelationNameCatalog<'a> {
    pub fn new(
        tables: &'a dyn RelationTableNames,
        definitions: &'a CatalogDefinitionSnapshot,
    ) -> Self {
        Self {
            tables,
            definitions,
        }
    }

    /// Produce the only relation candidate set exposed to SQL binding and execution. Dynamic names are filtered by namespace `USAGE`; stored bindings must already be canonical and therefore bypass name lookup entirely.
    pub(super) fn relation_lookup_candidates(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Vec<uqa_core::RelationIdentity>, SQLError> {
        if resolution.lookup_mode() == RelationLookupMode::Bound {
            return Ok(vec![uqa_sql::catalog::resolution::bound_relation_identity(
                name,
            )?]);
        }

        let qualified = uqa_core::RelationIdentity::parse_reference(name)
            .map_err(SQLError::Internal)?
            .0
            .is_some();
        let mut visible = Vec::new();
        for relation in resolution.raw_relation_lookup_candidates(name)? {
            if self
                .definitions
                .schemas
                .get(&relation.schema)
                .is_some_and(|security| {
                    !security
                        .resolve(&self.definitions.roles)
                        .is_ok_and(|security| {
                            crate::catalog::security::schema::role_has_schema_privilege(
                                &security,
                                &resolution.current_user,
                                crate::catalog::security::schema::SchemaAclPrivilege::Usage,
                                &self.definitions.roles,
                                &self.definitions.role_memberships,
                            )
                        })
                })
            {
                if qualified {
                    return Err(SQLError::Routine {
                        sqlstate: "42501".into(),
                        message: format!("permission denied for schema {}", relation.schema),
                    });
                }
                continue;
            }
            visible.push(relation);
        }
        Ok(visible)
    }

    fn namespace_exists(&self, resolution: &RelationNameResolution, schema: &str) -> bool {
        if schema == resolution.temporary_schema {
            return resolution.temporary_namespace_allocated;
        }
        uqa_sql::catalog::is_virtual_system_schema(schema)
            || self.definitions.schemas.contains_key(schema)
            || self.definitions.graphs.contains_key(schema)
    }

    pub fn relation_kind_resolution(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<RelationResolution, SQLError> {
        if resolution.lookup_mode() == RelationLookupMode::Dynamic {
            if let Some((requested, resolved)) = resolution.qualified_schema(name)? {
                if !self.namespace_exists(resolution, &resolved) {
                    return Ok(RelationResolution::MissingSchema(requested));
                }
            }
        }
        for relation in self.relation_lookup_candidates(resolution, name)? {
            let kind = if let Some(virtual_relation) =
                uqa_sql::catalog::SystemRelation::at(&relation.schema, &relation.name)
            {
                Some(virtual_relation.kind())
            } else if self.tables.contains_table(&relation) {
                Some("table")
            } else if let Some(view) = self.definitions.views.get(&relation) {
                Some(match view.kind {
                    crate::catalog::view::StoredViewKind::View => "view",
                    crate::catalog::view::StoredViewKind::Materialized => "materialized view",
                })
            } else if self.definitions.sequences.contains_key(&relation) {
                Some("sequence")
            } else if self.definitions.foreign_tables.contains_key(&relation) {
                Some("foreign table")
            } else if self.definitions.catalog_indexes.contains_key(&relation) {
                Some("index")
            } else if self
                .definitions
                .composites
                .contains_key(&relation.qualified_name())
            {
                Some("composite type")
            } else {
                None
            };
            if let Some(kind) = kind {
                return Ok(RelationResolution::Found(relation.qualified_name(), kind));
            }
        }
        Ok(RelationResolution::MissingRelation)
    }
}

#[cfg(test)]
mod tests;
