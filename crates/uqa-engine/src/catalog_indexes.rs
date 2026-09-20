//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    CatalogIndexRow, Engine, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult,
};

mod definition;
pub(crate) use definition::{index_definition, IndexDefinition};

impl Engine {
    pub fn register_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[String],
        options: &[(String, String)],
    ) -> StorageBackendResult<()> {
        self.try_register_catalog_index(name, index_type, table, columns, options)
    }

    pub(crate) fn try_register_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[String],
        options: &[(String, String)],
    ) -> StorageBackendResult<()> {
        self.register_catalog_index_definition(
            name,
            index_type,
            table,
            &columns
                .iter()
                .cloned()
                .map(uqa_sql::ast::IndexKey::Column)
                .collect::<Vec<_>>(),
            options,
            &IndexDefinition::default(),
        )
    }

    pub(crate) fn register_catalog_index_definition(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[uqa_sql::ast::IndexKey],
        options: &[(String, String)],
        definition: &IndexDefinition,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_register_catalog_index_inner(
                name, index_type, table, columns, options, definition,
            )
        })
    }

    fn try_register_catalog_index_inner(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[uqa_sql::ast::IndexKey],
        options: &[(String, String)],
        definition: &IndexDefinition,
    ) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()?;
        let (relation, table_relation) =
            uqa_execution::schema::indexes::registry::binding::registration(
                &self.index_registry_context(),
                name,
                table,
            )?;
        let table = table_relation.qualified_name();
        if let crate::capabilities::RelationResolution::Found(_, kind) = self
            .resolve_bound_relation_kind(&relation.qualified_name())
            .map_err(|error| StorageBackendError::Other(error.to_string()))?
        {
            if kind != "index" {
                return Err(StorageBackendError::Other(format!(
                    "relation `{}` already exists as a {kind}",
                    relation.qualified_name()
                )));
            }
        }
        let definition = uqa_execution::schema::indexes::registration::prepare(
            self.catalog_identity_reservation_context(),
            &relation,
            &table_relation,
            definition,
        )
        .map_err(|error| StorageBackendError::backend("index catalog identity", error))?;
        let columns_json = serde_json::to_string(columns).map_err(StorageBackendError::from)?;
        let options_map: std::collections::BTreeMap<String, String> =
            options.iter().cloned().collect();
        let parameters_json =
            serde_json::to_string(&options_map).map_err(StorageBackendError::from)?;
        let row = CatalogIndexRow {
            relation: relation.clone(),
            index_type: index_type.to_string(),
            table_name: table.clone(),
            columns_json: columns_json.clone(),
            parameters_json: parameters_json.clone(),
            definition_json: Some(serde_json::to_string(&definition)?),
        };
        uqa_execution::schema::indexes::registry::lifecycle::register(
            &self.index_registry_context(),
            row,
        )
    }

    pub fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.try_drop_catalog_index(name)
    }

    pub(crate) fn try_drop_catalog_index(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.with_implicit_storage_transaction(|engine| {
            let Some(relation) = engine.try_resolve_catalog_index_relation(name)? else {
                return Ok(None);
            };
            uqa_execution::schema::indexes::registry::binding::removal(
                &engine.index_registry_context(),
                &relation,
            )?;
            engine.try_drop_catalog_index_inner(&relation, false)
        })
    }

    pub(crate) fn try_drop_catalog_index_relation(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_drop_catalog_index_inner(relation, true)
        })
    }

    fn try_drop_catalog_index_inner(
        &self,
        relation: &RelationIdentity,
        cascade: bool,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.synchronize_catalog_registries()?;
        uqa_execution::schema::indexes::registry::lifecycle::remove(
            &self.index_registry_context(),
            relation,
            cascade,
        )
    }

    pub fn catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>> {
        let Some(relation) = self.try_resolve_catalog_index_relation(name)? else {
            return Ok(None);
        };
        self.bound_catalog_index(&relation.qualified_name())
    }

    pub fn has_catalog_index(&self, name: &str) -> StorageBackendResult<bool> {
        Ok(self.try_resolve_catalog_index_relation(name)?.is_some())
    }

    pub(crate) fn try_resolve_catalog_index_relation(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<RelationIdentity>> {
        self.synchronize_catalog_registries()?;
        let candidates = self.relation_lookup_candidates(name)?;
        let catalog = self.catalog_read_view();
        let indexes = self.durable.catalog_indexes.read();
        Ok(candidates.into_iter().find(|candidate| {
            indexes.contains_key(candidate) || catalog.has_constraint_index(candidate)
        }))
    }

    pub(crate) fn bound_catalog_index(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.synchronize_catalog_registries()?;
        let (schema, name) = RelationIdentity::parse_reference(canonical_name)
            .map_err(StorageBackendError::Other)?;
        let schema = schema.ok_or_else(|| {
            StorageBackendError::Other(format!(
                "bound index identity `{canonical_name}` is not schema-qualified"
            ))
        })?;
        let relation = RelationIdentity::new(schema, name);
        if let Some(index) = self.durable.catalog_indexes.read().get(&relation).cloned() {
            return Ok(Some(index));
        }
        self.catalog_read_view()
            .constraint_index(&relation)
            .map_err(|error| StorageBackendError::Other(error.to_string()))
    }

    /// An index has no independent owner: `PostgreSQL` derives its owner from the indexed table and additionally permits the containing schema's owner to drop it.
    pub(crate) fn require_index_drop_authority(
        &self,
        index: &CatalogIndexRow,
    ) -> Result<(), SQLError> {
        let table = RelationIdentity::from_legacy_name(&index.table_name)
            .map_err(|error| SQLError::Internal(format!("resolve indexed table: {error}")))?;
        let table_owner = self
            .storage
            .tables
            .read()
            .get(&table)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "index `{}` references missing table `{}`",
                    index.relation.qualified_name(),
                    index.table_name
                ))
            })?
            .role_owner();
        if self.current_user_has_role_privileges(&table_owner)
            || self
                .schema_security_for_privilege(&index.relation.schema)
                .is_some_and(|schema| self.current_user_has_role_privileges(&schema.role_owner))
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of index {}", index.relation.name),
        })
    }
}
