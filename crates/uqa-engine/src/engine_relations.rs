//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    value_to_f64_vec, value_to_usize, Arc, Engine, RelationIdentity, SQLError, StorageBackendError,
    StorageBackendResult, TableState, TrainingExample, TrainingSet,
};

impl Engine {
    fn relation_lookup_candidates(
        &self,
        name: &str,
    ) -> StorageBackendResult<Vec<RelationIdentity>> {
        let (schema, relation) =
            RelationIdentity::parse_reference(name).map_err(StorageBackendError::Other)?;
        if let Some(schema) = schema {
            return Ok(vec![RelationIdentity::new(schema, relation)]);
        }
        let mut candidates = Vec::new();
        for schema in &self.session.state.read().search_path {
            if schema == "pg_catalog" || schema == "information_schema" {
                continue;
            }
            candidates.push(RelationIdentity::new(schema, &relation));
        }
        Ok(candidates)
    }

    pub(crate) fn try_relation_name_for_create(&self, name: &str) -> Result<String, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh schema catalog: {err}"))?;
        let (schema, relation) = RelationIdentity::parse_reference(name)?;
        if let Some(schema) = schema {
            if !self.durable.schemas.read().contains(&schema) {
                return Err(format!("schema `{schema}` does not exist"));
            }
            return Ok(RelationIdentity::new(schema, relation).qualified_name());
        }
        let session = self.session.state.read();
        let schemas = self.durable.schemas.read();
        let schema = session
            .search_path
            .iter()
            .find(|schema| {
                schema.as_str() != "pg_catalog"
                    && schema.as_str() != "information_schema"
                    && schemas.contains(schema.as_str())
            })
            .cloned()
            .ok_or_else(|| "no schema has been selected to create in".to_string())?;
        Ok(RelationIdentity::new(schema, relation).qualified_name())
    }

    pub(crate) fn relation_kind_at(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<Option<&'static str>> {
        self.synchronize_table_catalog()?;
        self.synchronize_catalog_registries()?;
        self.refresh_sequences_from_catalog()?;
        let relation = RelationIdentity::from_legacy_name(canonical_name)
            .map_err(StorageBackendError::Other)?;
        if self.storage.tables.read().contains_key(&relation) {
            Ok(Some("table"))
        } else if self.durable.views.read().contains_key(&relation) {
            Ok(Some("view"))
        } else if self.durable.sequences.read().contains_key(&relation) {
            Ok(Some("sequence"))
        } else if self.durable.foreign_tables.read().contains_key(&relation) {
            Ok(Some("foreign table"))
        } else {
            Ok(None)
        }
    }

    /// Resolve one name through the shared relation namespace, retaining its
    /// concrete kind. `IF EXISTS` callers use this to distinguish a genuinely
    /// absent object from an object of the wrong kind.
    pub(crate) fn try_resolve_relation_kind(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<(String, &'static str)>> {
        self.synchronize_table_catalog()?;
        self.synchronize_catalog_registries()?;
        self.refresh_sequences_from_catalog()?;
        for relation in self.relation_lookup_candidates(name)? {
            let kind = if self.storage.tables.read().contains_key(&relation) {
                Some("table")
            } else if self.durable.views.read().contains_key(&relation) {
                Some("view")
            } else if self.durable.sequences.read().contains_key(&relation) {
                Some("sequence")
            } else if self.durable.foreign_tables.read().contains_key(&relation) {
                Some("foreign table")
            } else {
                None
            };
            if let Some(kind) = kind {
                return Ok(Some((relation.qualified_name(), kind)));
            }
        }
        Ok(None)
    }

    pub(crate) fn resolved_relation_identity(name: &str) -> StorageBackendResult<RelationIdentity> {
        RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)
    }

    pub(crate) fn try_resolve_table_name(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<String>> {
        self.synchronize_table_catalog()?;
        self.synchronize_table_data()?;
        let tables = self.storage.tables.read();
        Ok(self
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
            .map(|relation| relation.qualified_name()))
    }

    pub(crate) fn resolve_table_name(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.try_resolve_table_name(name)
    }

    pub(crate) fn try_resolve_view_name(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.synchronize_catalog_registries()?;
        let views = self.durable.views.read();
        Ok(self
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|candidate| views.contains_key(candidate))
            .map(|relation| relation.qualified_name()))
    }

    pub(crate) fn try_resolve_sequence_name(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<String>> {
        self.refresh_sequences_from_catalog()?;
        let sequences = self.durable.sequences.read();
        Ok(self
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|candidate| sequences.contains_key(candidate))
            .map(|relation| relation.qualified_name()))
    }

    pub(crate) fn resolve_foreign_table_name(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<String>> {
        self.synchronize_catalog_registries()?;
        let tables = self.durable.foreign_tables.read();
        Ok(self
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
            .map(|relation| relation.qualified_name()))
    }

    pub(crate) fn table(&self, name: &str) -> StorageBackendResult<Option<Arc<TableState>>> {
        let Some(resolved) = self.resolve_table_name(name)? else {
            return Ok(None);
        };
        let relation =
            RelationIdentity::from_legacy_name(&resolved).map_err(StorageBackendError::Other)?;
        Ok(self.storage.tables.read().get(&relation).cloned())
    }

    pub(crate) fn try_table(&self, name: &str) -> StorageBackendResult<Option<Arc<TableState>>> {
        let Some(resolved) = self.try_resolve_table_name(name)? else {
            return Ok(None);
        };
        let relation =
            RelationIdentity::from_legacy_name(&resolved).map_err(StorageBackendError::Other)?;
        Ok(self.storage.tables.read().get(&relation).cloned())
    }

    pub(crate) fn require_table(&self, name: &str) -> Result<Arc<TableState>, SQLError> {
        self.try_table(name)
            .map_err(|err| SQLError::Internal(format!("resolve table `{name}`: {err}")))?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
    }

    pub(crate) fn training_set_from_table(
        &self,
        table: &str,
        features_field: &str,
        label_field: &str,
    ) -> Result<TrainingSet, SQLError> {
        let table_state = self
            .try_table(table)
            .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let store = table_state.document_store.read();
        let documents = store
            .iter_all()
            .map_err(|err| SQLError::Internal(format!("scan deep_learn table `{table}`: {err}")))?;
        let mut examples = Vec::new();
        for (doc_id, document) in documents {
            let features = document.get(features_field).ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "deep_learn table {table:?} row {doc_id} is missing `{features_field}`"
                ))
            })?;
            let label = document.get(label_field).ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "deep_learn table {table:?} row {doc_id} is missing `{label_field}`"
                ))
            })?;
            examples.push(TrainingExample {
                features: value_to_f64_vec(features).map_err(|e| {
                    SQLError::TypeMismatch(format!(
                        "deep_learn table {table:?} row {doc_id} `{features_field}`: {e}"
                    ))
                })?,
                label: value_to_usize(label).map_err(|e| {
                    SQLError::TypeMismatch(format!(
                        "deep_learn table {table:?} row {doc_id} `{label_field}`: {e}"
                    ))
                })?,
            });
        }
        Ok(TrainingSet {
            examples,
            class_count: None,
        })
    }
}
