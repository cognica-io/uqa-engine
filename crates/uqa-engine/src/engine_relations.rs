//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Arc, Engine, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult, TableState,
    TrainingExample, TrainingSet,
};

pub(super) fn value_to_f64_vec(value: &super::Value) -> Result<Vec<f64>, String> {
    match value {
        super::Value::List(items) => items
            .iter()
            .map(|item| match item {
                super::Value::Float(value) => Ok(*value),
                super::Value::Int(value) => Ok(*value as f64),
                super::Value::Decimal(value) => value
                    .to_f64()
                    .ok_or_else(|| "decimal feature is outside f64 range".to_string()),
                other => Err(format!("expected numeric feature, got {other:?}")),
            })
            .collect(),
        super::Value::Array(array) if array.dimensions().len() <= 1 => array
            .elements()
            .iter()
            .map(|item| match item {
                super::Value::Float(value) => Ok(*value),
                super::Value::Int(value) => Ok(*value as f64),
                super::Value::Decimal(value) => value
                    .to_f64()
                    .ok_or_else(|| "decimal feature is outside f64 range".to_string()),
                other => Err(format!("expected numeric feature, got {other:?}")),
            })
            .collect(),
        super::Value::Array(array) => Err(format!(
            "expected one-dimensional feature array, got {} dimensions",
            array.dimensions().len()
        )),
        other => Err(format!("expected feature array, got {other:?}")),
    }
}

pub(super) fn value_to_usize(value: &super::Value) -> Result<usize, String> {
    match value {
        super::Value::Int(value) if *value >= 0 => usize::try_from(*value)
            .map_err(|_| format!("integer label {value} exceeds the platform usize range")),
        super::Value::Float(value) => {
            let exponent = i32::try_from(usize::BITS)
                .map_err(|_| "platform usize width exceeds f64 exponent range".to_string())?;
            let upper_exclusive = 2.0_f64.powi(exponent);
            if !value.is_finite()
                || *value < 0.0
                || value.fract() != 0.0
                || *value >= upper_exclusive
            {
                return Err(format!(
                    "expected finite non-negative integer label within usize range, got {value}"
                ));
            }
            Ok(*value as usize)
        }
        other => Err(format!(
            "expected non-negative integer label, got {other:?}"
        )),
    }
}

impl Engine {
    pub(crate) fn relation_lookup_candidates(
        &self,
        name: &str,
    ) -> StorageBackendResult<Vec<RelationIdentity>> {
        let (schema, relation) =
            RelationIdentity::parse_reference(name).map_err(StorageBackendError::Other)?;
        if let Some(schema) = schema {
            if schema == "pg_temp" {
                return Ok(vec![RelationIdentity::new(
                    self.temporary_schema_name(),
                    relation,
                )]);
            }
            return Ok(vec![RelationIdentity::new(schema, relation)]);
        }
        let mut candidates = Vec::new();
        candidates.push(RelationIdentity::new(
            self.temporary_schema_name(),
            &relation,
        ));
        for schema in &self.session.state.read().search_path {
            if schema == "pg_catalog" || schema == "information_schema" {
                continue;
            }
            candidates.push(RelationIdentity::new(schema, &relation));
        }
        Ok(candidates)
    }

    pub(crate) fn ensure_temporary_relation_creation_privilege(&self) -> Result<(), SQLError> {
        let current_user = self.current_user_name();
        self.ensure_database_privilege(
            &current_user,
            crate::engine_database_security::DatabaseAclPrivilege::Temporary,
        )
    }

    pub(crate) fn try_temporary_relation_name_for_create(
        &self,
        name: &str,
    ) -> Result<String, SQLError> {
        self.ensure_temporary_relation_creation_privilege()?;
        let (schema, relation) =
            RelationIdentity::parse_reference(name).map_err(SQLError::Unsupported)?;
        let temporary_schema = self.temporary_schema_name();
        if schema
            .as_deref()
            .is_some_and(|schema| schema != "pg_temp" && schema != temporary_schema)
        {
            return Err(SQLError::Unsupported(
                "temporary relations cannot specify a schema name".into(),
            ));
        }
        self.session.state.write().temporary_namespace_allocated = true;
        Ok(RelationIdentity::new(temporary_schema, relation).qualified_name())
    }

    pub(crate) fn temporary_namespace_allocated(&self) -> bool {
        self.session.state.read().temporary_namespace_allocated
    }

    pub(crate) fn temporary_schema_name(&self) -> String {
        format!("pg_temp_{}", self.session_id)
    }

    pub(crate) fn try_relation_name_for_create(&self, name: &str) -> Result<String, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh schema catalog: {err}"))?;
        let (schema, relation) = RelationIdentity::parse_reference(name)?;
        if let Some(schema) = schema {
            if !self.durable.schemas.read().contains_key(&schema) {
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
                    && schemas.contains_key(schema.as_str())
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
        } else if let Some(view) = self.durable.views.read().get(&relation) {
            Ok(Some(match view.kind {
                super::StoredViewKind::View => "view",
                super::StoredViewKind::Materialized => "materialized view",
            }))
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
            } else if let Some(view) = self.durable.views.read().get(&relation) {
                Some(match view.kind {
                    super::StoredViewKind::View => "view",
                    super::StoredViewKind::Materialized => "materialized view",
                })
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

    /// Resolve a relation for a sequence operation while applying schema `USAGE` to each search-path candidate. A qualified inaccessible schema reports `42501`; an unqualified lookup skips it as `PostgreSQL` does when constructing the effective search path.
    pub(crate) fn try_resolve_sequence_relation_kind(
        &self,
        name: &str,
        role: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        self.synchronize_table_catalog().map_err(|error| {
            SQLError::Internal(format!("load table catalog for sequence lookup: {error}"))
        })?;
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("load schema catalog for sequence lookup: {error}"))
        })?;
        self.refresh_sequences_from_catalog().map_err(|error| {
            SQLError::Internal(format!("load sequence catalog for lookup: {error}"))
        })?;
        let qualified = RelationIdentity::parse_reference(name)
            .map_err(SQLError::Internal)?
            .0
            .is_some();
        for relation in self
            .relation_lookup_candidates(name)
            .map_err(|error| SQLError::Internal(format!("resolve relation `{name}`: {error}")))?
        {
            if self.durable.schemas.read().contains_key(&relation.schema)
                && !self.schema_has_privilege_for_role(
                    &relation.schema,
                    role,
                    crate::engine_schema_security::SchemaAclPrivilege::Usage,
                )
            {
                if qualified {
                    return Err(SQLError::Routine {
                        sqlstate: "42501".into(),
                        message: format!("permission denied for schema {}", relation.schema),
                    });
                }
                continue;
            }
            let kind = if self.storage.tables.read().contains_key(&relation) {
                Some("table")
            } else if let Some(view) = self.durable.views.read().get(&relation) {
                Some(match view.kind {
                    super::StoredViewKind::View => "view",
                    super::StoredViewKind::Materialized => "materialized view",
                })
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

    /// `PostgreSQL` reports a missing qualified namespace separately for sequence DDL and privilege inquiry, while value functions and direct relation scans retain an undefined-relation error.
    pub(crate) fn ensure_sequence_reference_schema_exists(
        &self,
        name: &str,
    ) -> Result<(), SQLError> {
        let (schema, _) = RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
        let Some(schema) = schema else {
            return Ok(());
        };
        let temporary_schema = self.temporary_schema_name();
        let exists = if schema == "pg_temp" || schema == temporary_schema {
            self.temporary_namespace_allocated()
        } else {
            self.has_namespace(&schema).map_err(|error| {
                SQLError::Internal(format!(
                    "resolve schema `{schema}` for sequence lookup: {error}"
                ))
            })?
        };
        if exists {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{schema}\" does not exist"),
        })
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

    pub(crate) fn try_resolve_query_table_name(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<String>> {
        if let Some(snapshot) = self.query_table_snapshots.as_ref() {
            return Ok(self
                .relation_lookup_candidates(name)?
                .into_iter()
                .find(|candidate| snapshot.contains_key(candidate))
                .map(|relation| relation.qualified_name()));
        }
        self.try_resolve_table_name(name)
    }

    pub(crate) fn try_resolve_view_name(&self, name: &str) -> StorageBackendResult<Option<String>> {
        if let Some(snapshot) = self.query_view_snapshots.as_ref() {
            return Ok(self
                .relation_lookup_candidates(name)?
                .into_iter()
                .find(|candidate| snapshot.contains_key(candidate))
                .map(|relation| relation.qualified_name()));
        }
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

    pub(crate) fn try_query_table(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<Arc<TableState>>> {
        let Some(resolved) = self.try_resolve_query_table_name(name)? else {
            return Ok(None);
        };
        let relation =
            RelationIdentity::from_legacy_name(&resolved).map_err(StorageBackendError::Other)?;
        let live = self.storage.tables.read().get(&relation).cloned();
        if let Some(snapshot) = self.query_table_snapshots.as_ref() {
            if let Some(table) = snapshot.get(&relation) {
                let table = Arc::clone(table);
                let changes = self
                    .fixed_transaction_row_changes(&resolved)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                return match changes.as_ref() {
                    Some(changes) if !changes.is_empty() => {
                        Self::detach_query_table(&table, &table, Some(changes))
                            .map(Some)
                            .map_err(|error| StorageBackendError::Other(error.to_string()))
                    }
                    _ => Ok(Some(table)),
                };
            }
        }
        if live
            .as_ref()
            .is_some_and(|table| table.persistence == uqa_sql::ast::RelationPersistence::Temporary)
        {
            return Ok(live);
        }
        let (fixed_snapshot_set, snapshot_table) =
            self.session
                .transactions
                .lock()
                .first()
                .map_or((false, None), |frame| {
                    let Some(snapshot) = frame.fixed_snapshot.as_ref() else {
                        return (false, None);
                    };
                    let table = live.as_ref().map_or_else(
                        || snapshot.table(&relation),
                        |table| snapshot.table_for_live_relation(&relation, table),
                    );
                    (true, table)
                });
        let Some(snapshot_table) = snapshot_table else {
            if fixed_snapshot_set
                && live.as_ref().is_some_and(|table| {
                    table.persistence != uqa_sql::ast::RelationPersistence::Temporary
                })
            {
                let changes = self
                    .fixed_transaction_row_changes(&resolved)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                if changes.as_ref().is_some_and(|changes| !changes.is_empty()) {
                    return Ok(live);
                }
                return live
                    .as_ref()
                    .map(Self::detach_empty_query_table)
                    .transpose()
                    .map_err(|error| StorageBackendError::Other(error.to_string()));
            }
            return Ok(live);
        };
        let Some(metadata) = live.as_ref() else {
            return Ok(Some(snapshot_table));
        };
        let changes = self
            .fixed_transaction_row_changes(&resolved)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        let metadata_changed = Self::table_catalog_metadata_fingerprint(&snapshot_table)?
            != Self::table_catalog_metadata_fingerprint(metadata)?;
        match (changes.as_ref(), metadata_changed) {
            (Some(changes), _) if !changes.is_empty() => {
                Self::detach_query_table(&snapshot_table, metadata, Some(changes))
                    .map(Some)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))
            }
            (_, true) => Self::detach_query_table(&snapshot_table, metadata, None)
                .map(Some)
                .map_err(|error| StorageBackendError::Other(error.to_string())),
            _ => Ok(Some(snapshot_table)),
        }
    }

    pub(crate) fn require_table(&self, name: &str) -> Result<Arc<TableState>, SQLError> {
        self.try_table(name)
            .map_err(|err| SQLError::Internal(format!("resolve table `{name}`: {err}")))?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
    }

    pub(crate) fn require_query_table(&self, name: &str) -> Result<Arc<TableState>, SQLError> {
        self.try_query_table(name)
            .map_err(|error| SQLError::Internal(format!("resolve query table `{name}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
    }

    pub(crate) fn table_persistence(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::RelationPersistence>> {
        Ok(self.try_query_table(name)?.map(|table| table.persistence))
    }

    pub(crate) fn sequence_persistence(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::RelationPersistence>> {
        let Some(name) = self.try_resolve_sequence_name(name)? else {
            return Ok(None);
        };
        let relation = Self::resolved_relation_identity(&name)?;
        Ok(self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .copied())
    }

    pub(crate) fn sequence_object_id(&self, name: &str) -> StorageBackendResult<Option<[u8; 16]>> {
        let Some(name) = self.try_resolve_sequence_name(name)? else {
            return Ok(None);
        };
        let relation = Self::resolved_relation_identity(&name)?;
        Ok(self
            .durable
            .sequence_object_ids
            .read()
            .get(&relation)
            .copied())
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
        let doc_ids =
            table_state.document_store.read().doc_ids().map_err(|err| {
                SQLError::Internal(format!("scan deep_learn table `{table}`: {err}"))
            })?;
        let projection = vec![features_field.to_string(), label_field.to_string()];
        let documents = self.get_documents_with_virtual_projection(table, &doc_ids, &projection)?;
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
