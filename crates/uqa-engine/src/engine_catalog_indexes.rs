//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    CatalogIndexRow, Engine, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult,
};

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
        self.with_implicit_storage_transaction(|engine| {
            engine.try_register_catalog_index_inner(name, index_type, table, columns, options)
        })
    }

    fn try_register_catalog_index_inner(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[String],
        options: &[(String, String)],
    ) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()?;
        let table = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        let table_relation =
            RelationIdentity::from_legacy_name(&table).map_err(StorageBackendError::Other)?;
        let (requested_schema, local_name) =
            RelationIdentity::parse_reference(name).map_err(StorageBackendError::Other)?;
        if requested_schema
            .as_deref()
            .is_some_and(|schema| schema != table_relation.schema)
        {
            return Err(StorageBackendError::Other(format!(
                "index `{name}` cannot belong to a different schema than table `{table}`"
            )));
        }
        let relation = RelationIdentity::new(&table_relation.schema, local_name);
        if let crate::engine_capabilities::RelationResolution::Found(_, kind) = self
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
        let persistence = self
            .storage
            .tables
            .read()
            .get(&table_relation)
            .map(|table| table.persistence)
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
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
        };
        let previous = self
            .durable
            .catalog_indexes
            .write()
            .insert(relation.clone(), row.clone());
        if let Err(err) = self.refresh_catalog_index_tables(&row, previous.as_ref()) {
            self.restore_catalog_index_entry(&relation, previous.as_ref());
            if let Err(cleanup) = self.restore_catalog_index_tables(&row, previous.as_ref()) {
                return Err(StorageBackendError::Other(format!(
                    "{err}; restoring value indexes after the index build failure also failed: {cleanup}"
                )));
            }
            return Err(err);
        }
        if persistence != uqa_sql::ast::RelationPersistence::Temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                if let Err(err) = catalog.save_catalog_index(
                    &relation,
                    index_type,
                    &table,
                    &columns_json,
                    &parameters_json,
                ) {
                    self.restore_catalog_index_entry(&relation, previous.as_ref());
                    if let Err(cleanup) = self.restore_catalog_index_tables(&row, previous.as_ref())
                    {
                        return Err(StorageBackendError::Other(format!(
                            "{err}; restoring value indexes after the catalog write failure also failed: {cleanup}"
                        )));
                    }
                    return Err(err);
                }
                self.note_table_catalog_changed();
            }
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    fn restore_catalog_index_tables(
        &self,
        current: &CatalogIndexRow,
        previous: Option<&CatalogIndexRow>,
    ) -> StorageBackendResult<()> {
        let mut tables = std::collections::BTreeSet::new();
        for row in std::iter::once(current).chain(previous) {
            if row.index_type.eq_ignore_ascii_case("btree") {
                tables.insert(row.table_name.as_str());
            }
        }
        for table in tables {
            if self.try_resolve_table_name(table)?.is_some() {
                self.refresh_value_indexes_for_table(table)?;
            }
        }
        Ok(())
    }

    fn restore_catalog_index_entry(
        &self,
        relation: &RelationIdentity,
        previous: Option<&CatalogIndexRow>,
    ) {
        let mut indexes = self.durable.catalog_indexes.write();
        indexes.remove(relation);
        if let Some(previous) = previous {
            indexes.insert(relation.clone(), previous.clone());
        }
    }

    fn refresh_catalog_index_tables(
        &self,
        current: &CatalogIndexRow,
        previous: Option<&CatalogIndexRow>,
    ) -> StorageBackendResult<()> {
        let mut tables = std::collections::BTreeSet::new();
        for row in std::iter::once(current).chain(previous) {
            if row.index_type.eq_ignore_ascii_case("btree") {
                tables.insert(row.table_name.as_str());
            }
        }
        for table in tables {
            self.refresh_value_indexes_for_table(table)?;
        }
        Ok(())
    }

    pub fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.try_drop_catalog_index(name)
    }

    pub(crate) fn try_drop_catalog_index(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        let Some(relation) = self.try_resolve_catalog_index_relation(name)? else {
            return Ok(None);
        };
        self.try_drop_catalog_index_relation(&relation)
    }

    pub(crate) fn try_drop_catalog_index_relation(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_drop_catalog_index_inner(relation)
        })
    }

    fn try_drop_catalog_index_inner(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.synchronize_catalog_registries()?;
        let existing = self.durable.catalog_indexes.read().get(relation).cloned();
        let Some(existing_row) = existing else {
            return Ok(None);
        };
        let removed = self.durable.catalog_indexes.write().remove(relation);
        if existing_row.index_type.eq_ignore_ascii_case("btree") {
            if let Err(err) = self.refresh_value_indexes_for_table(&existing_row.table_name) {
                self.durable
                    .catalog_indexes
                    .write()
                    .insert(relation.clone(), existing_row.clone());
                if let Err(cleanup) = self.refresh_value_indexes_for_table(&existing_row.table_name)
                {
                    return Err(StorageBackendError::Other(format!(
                        "{err}; restoring value indexes after the index drop failure also failed: {cleanup}"
                    )));
                }
                return Err(err);
            }
        }
        let temporary = RelationIdentity::from_legacy_name(&existing_row.table_name)
            .ok()
            .and_then(|table| self.storage.tables.read().get(&table).cloned())
            .is_some_and(|table| table.persistence == uqa_sql::ast::RelationPersistence::Temporary);
        if !temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                if let Err(err) = catalog.drop_catalog_index(relation) {
                    self.durable
                        .catalog_indexes
                        .write()
                        .insert(relation.clone(), existing_row.clone());
                    if existing_row.index_type.eq_ignore_ascii_case("btree") {
                        if let Err(cleanup) =
                            self.refresh_value_indexes_for_table(&existing_row.table_name)
                        {
                            return Err(StorageBackendError::Other(format!(
                                "{err}; restoring value indexes after the catalog delete failure also failed: {cleanup}"
                            )));
                        }
                    }
                    return Err(err);
                }
                self.note_table_catalog_changed();
            }
        }
        self.note_catalog_registry_changed();
        Ok(removed)
    }

    pub fn catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>> {
        let Some(relation) = self.try_resolve_catalog_index_relation(name)? else {
            return Ok(None);
        };
        Ok(self.durable.catalog_indexes.read().get(&relation).cloned())
    }

    pub fn has_catalog_index(&self, name: &str) -> StorageBackendResult<bool> {
        Ok(self.try_resolve_catalog_index_relation(name)?.is_some())
    }

    pub(crate) fn try_resolve_catalog_index_relation(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<RelationIdentity>> {
        self.synchronize_catalog_registries()?;
        let indexes = self.durable.catalog_indexes.read();
        Ok(self
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|candidate| indexes.contains_key(candidate)))
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
        Ok(self
            .durable
            .catalog_indexes
            .read()
            .get(&RelationIdentity::new(schema, name))
            .cloned())
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
