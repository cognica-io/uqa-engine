//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Arc, Engine, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult, TableState,
};
use crate::capabilities::RelationResolution;

impl Engine {
    pub(crate) fn rewrite_relation_rename_dependents(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        uqa_execution::schema::relation_alteration::rewrite_relation_rename_dependents(
            self, from, to,
        )
    }

    pub(crate) fn relation_lookup_candidates(
        &self,
        name: &str,
    ) -> StorageBackendResult<Vec<RelationIdentity>> {
        uqa_sql::catalog::resolution::candidates::relation_lookup_candidates(self, name)
            .map_err(StorageBackendError::Other)
    }

    pub(crate) fn temporary_namespace_allocated(&self) -> bool {
        self.session.state.read().temporary_namespace_allocated
    }

    pub(crate) fn temporary_schema_name(&self) -> String {
        format!("pg_temp_{}", self.session_id)
    }

    pub(crate) fn relation_kind_at(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<Option<&'static str>> {
        self.synchronize_table_catalog()?;
        self.synchronize_catalog_registries()?;
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
        } else if self.durable.catalog_indexes.read().contains_key(&relation) {
            Ok(Some("index"))
        } else {
            Ok(None)
        }
    }

    /// Resolve a SQL relation reference through the current user's effective namespace while preserving whether a qualified namespace or only the relation was absent.
    pub(crate) fn resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<RelationResolution, SQLError> {
        self.resolve_relation_kind_for_query(name, false)
    }

    /// Resolve a canonical relation identity against the already-loaded catalog without consulting the active role's namespace or recursively synchronizing registries.
    pub(crate) fn resolve_bound_relation_kind(
        &self,
        name: &str,
    ) -> Result<RelationResolution, SQLError> {
        let mut resolution = self.session_execution_view().relation_name_resolution();
        resolution.set_lookup_mode(crate::capabilities::RelationLookupMode::Bound);
        self.catalog_read_view()
            .relation_kind_resolution(&resolution, name)
    }

    /// Resolve a dynamic name against registries that the caller has already synchronized. Catalog restoration uses this entry point while holding the synchronization boundary, so dependency binding cannot recursively reload the catalog.
    pub(crate) fn resolve_loaded_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<RelationResolution, SQLError> {
        let resolution = self.session_execution_view().relation_name_resolution();
        self.catalog_read_view()
            .relation_kind_resolution(&resolution, name)
    }

    /// Resolve a SQL relation reference through the current user's effective namespace. This is the dynamic-name boundary; code operating on a returned canonical identity must use exact catalog access rather than repeating name resolution.
    pub(crate) fn try_resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        Ok(self.resolve_visible_relation_kind(name)?.into_found())
    }

    /// Bind a table named as a secondary DDL relation, where `PostgreSQL` distinguishes an absent qualified schema from an absent relation.
    pub(crate) fn resolve_visible_table_reference(&self, name: &str) -> Result<String, SQLError> {
        match self.resolve_visible_relation_kind(name)? {
            RelationResolution::Found(canonical, "table") => Ok(canonical),
            RelationResolution::MissingSchema(schema) => Err(SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            }),
            RelationResolution::Found(_, _) | RelationResolution::MissingRelation => {
                Err(SQLError::UnknownTable(name.to_string()))
            }
        }
    }

    pub(crate) fn try_resolve_bound_table_name(
        &self,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        Ok(match self.resolve_bound_relation_kind(name)? {
            RelationResolution::Found(name, "table") => Some(name),
            RelationResolution::Found(_, _)
            | RelationResolution::MissingRelation
            | RelationResolution::MissingSchema(_) => None,
        })
    }

    /// Resolve a query source with the namespace semantics recorded by its owning plan. Dynamic plans use the current effective namespace; stored plans accept only canonical identities captured by their binder.
    pub(crate) fn resolve_relation_kind_for_query(
        &self,
        name: &str,
        relations_bound: bool,
    ) -> Result<RelationResolution, SQLError> {
        // Registry synchronization hydrates sequence identities with every other relation kind. Sequence-value refresh is intentionally separate because name binding neither observes nor mutates nontransactional sequence state.
        self.synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("load table catalog: {error}")))?;
        self.synchronize_catalog_registries()
            .map_err(|error| SQLError::Internal(format!("load relation catalog: {error}")))?;
        let mut resolution = self.session_execution_view().relation_name_resolution();
        if relations_bound {
            resolution.set_lookup_mode(crate::capabilities::RelationLookupMode::Bound);
        }
        self.catalog_read_view()
            .relation_kind_resolution(&resolution, name)
    }

    pub(crate) fn try_resolve_relation_kind_for_query(
        &self,
        name: &str,
        relations_bound: bool,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        Ok(self
            .resolve_relation_kind_for_query(name, relations_bound)?
            .into_found())
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
            } else if self.durable.catalog_indexes.read().contains_key(&relation) {
                Some("index")
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
}

impl Engine {
    pub(crate) fn resolve_mutation_target_name(
        &self,
        name: &str,
        target_relation_bound: bool,
    ) -> Result<String, SQLError> {
        let resolution = if target_relation_bound {
            self.resolve_bound_relation_kind(name)?.into_found()
        } else {
            self.try_resolve_visible_relation_kind(name)?
        };
        resolution
            .map(|(canonical, _)| canonical)
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
    }
}

impl Engine {
    pub(crate) fn mutation_view_kind(
        &self,
        name: &str,
    ) -> Result<Option<crate::StoredViewKind>, SQLError> {
        let candidates = self.relation_lookup_candidates(name).map_err(|error| {
            SQLError::Internal(format!("resolve DML relation `{name}`: {error}"))
        })?;
        let tables = self.storage.tables.read();
        let views = self.durable.views.read();
        for relation in candidates {
            if tables.contains_key(&relation) {
                return Ok(None);
            }
            if let Some(view) = views.get(&relation) {
                return Ok(Some(view.kind));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
