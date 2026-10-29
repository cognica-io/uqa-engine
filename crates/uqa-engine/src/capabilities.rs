//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Narrow capabilities over the engine's state ownership domains.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use uqa_sql::catalog::roles::RoleReference;

use uqa_sql::ast::TransactionIsolationLevel;
use uqa_sql::SQLError;
use uqa_storage::{StorageBackendError, StorageBackendResult};

use super::state::{
    DurableCatalogSnapshot, DurableCatalogState, EpochCoordinator, QueryRuntime, SessionContext,
    StorageContext,
};
use super::Engine;

pub(crate) use uqa_sql::catalog::resolution::{RelationLookupMode, RelationNameResolution};

/// Stable catalog generations observed by one statement boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogEpochs {
    pub(crate) table_catalog: u64,
    pub(crate) table_data: u64,
    pub(crate) catalog_registry: u64,
}

use uqa_execution::catalog::CatalogDefinitionSnapshot;
pub(crate) use uqa_execution::catalog::{CatalogReadView, RelationResolution};

/// Read-only session values visible to statement execution. Durable registries and storage backends are intentionally absent.
#[derive(Clone, Copy)]
pub(crate) struct SessionExecutionView<'a> {
    session: &'a SessionContext,
    /// The roles, which decide whether the session's role is a superuser (`is_superuser`).
    durable: &'a DurableCatalogState,
    session_id: u64,
    query_transaction_origin: Option<u64>,
}

impl SessionExecutionView<'_> {
    pub(crate) fn cursors(&self) -> Vec<uqa_sql::catalog::session::CursorMetadata> {
        self.session.portal_registry.snapshot()
    }

    pub(crate) fn prepared_statements(
        &self,
    ) -> Vec<super::statement_cache::PreparedStatementMetadata> {
        self.session
            .prepared
            .read()
            .iter()
            .filter(|(name, _)| !name.is_empty())
            .map(|(name, entry)| entry.metadata(name))
            .collect()
    }

    pub(crate) fn current_role(&self) -> RoleReference {
        RoleReference::Bound(self.session.state.read().authorization.current().clone())
    }

    pub(crate) fn session_role(&self) -> RoleReference {
        RoleReference::Bound(self.session.state.read().authorization.session().clone())
    }

    pub(crate) fn transaction_depth(&self) -> usize {
        self.session.transactions.lock().len()
    }

    pub(crate) fn transaction_snapshot_identity(&self) -> Option<u64> {
        self.query_transaction_origin
    }

    pub(crate) fn temporary_schema_name(&self) -> String {
        format!("pg_temp_{}", self.session_id)
    }

    pub(crate) fn relation_name_resolution(&self) -> RelationNameResolution {
        let state = self.session.state.read();
        RelationNameResolution {
            search_path: crate::session::effective_search_path(&state),
            temporary_schema: self.temporary_schema_name(),
            temporary_namespace_allocated: state.temporary_namespace.is_some(),
            current_user: RoleReference::Bound(state.authorization.current().clone()),
            lookup_mode: RelationLookupMode::Dynamic,
        }
    }

    /// The setting of a transaction characteristic `name`, and whether the transaction assigned it itself.
    pub(crate) fn transaction_parameter_setting(&self, name: &str) -> Option<(String, bool)> {
        let current = self.session.transactions.lock().last().map_or_else(
            || default_transaction_characteristics(self.session),
            |frame| frame.characteristics,
        );
        let (setting, flag) = match name {
            "transaction_isolation" => (
                current.isolation.as_str().to_string(),
                super::TransactionCharacteristicsState::ISOLATION_ASSIGNED,
            ),
            "transaction_read_only" => (
                if current.read_only { "on" } else { "off" }.to_string(),
                super::TransactionCharacteristicsState::READ_ONLY_ASSIGNED,
            ),
            "transaction_deferrable" => (
                if current.deferrable { "on" } else { "off" }.to_string(),
                super::TransactionCharacteristicsState::DEFERRABLE_ASSIGNED,
            ),
            _ => return None,
        };
        Some((setting, current.assigned & flag != 0))
    }
}

pub(crate) use uqa_execution::query::runtime::QueryRuntimeView;

impl uqa_execution::query::runtime::QueryMemorySettings for SessionContext {
    fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        let limit = self.query_memory_limit.load(Ordering::Acquire);
        if limit != 0 {
            return Ok(limit);
        }
        let setting = self.setting("work_mem");
        let kilobytes = setting.parse::<usize>().map_err(|_| {
            SQLError::Internal(format!(
                "work_mem holds a setting that is not an integer: {setting:?}"
            ))
        })?;
        Ok(kilobytes.saturating_mul(1024))
    }
}

/// Statement mutation owner over the existing storage, durable catalog, session, epoch, and runtime domains. It exposes command-specific transitions rather than the enclosing engine facade.
pub(crate) struct MutationCoordinator<'a> {
    storage: &'a StorageContext,
    durable: &'a DurableCatalogState,
    session: &'a SessionContext,
    epochs: &'a EpochCoordinator,
    runtime: &'a QueryRuntime,
}

impl MutationCoordinator<'_> {
    pub(crate) fn begin_command_mutation_overlay(&self) {
        self.session
            .command_mutation_overlays
            .lock()
            .push(super::CommandMutationOverlay::default());
    }

    pub(crate) fn end_command_mutation_overlay(&self) -> super::CommandMutationOverlay {
        self.session
            .command_mutation_overlays
            .lock()
            .pop()
            .expect("command mutation overlay stack underflow")
    }

    pub(crate) fn register_schema(
        &self,
        name: &str,
        if_not_exists: bool,
        role_owner: uqa_core::catalog_role::RoleIdentity,
        tuple: uqa_core::catalog_schema::SchemaTupleIdentity,
    ) -> StorageBackendResult<bool> {
        uqa_execution::schema::namespaces::register_schema(
            &uqa_execution::schema::namespaces::SchemaRegistrationContext {
                state: self,
                persistence: self,
                changes: self,
            },
            name,
            if_not_exists,
            role_owner,
            tuple,
        )
    }

    pub(crate) fn note_catalog_registry_changed(&self) {
        self.note_catalog_registry_change(
            uqa_execution::statement::prepared::invalidation::CatalogRegistryChange::Definitions,
        );
    }

    pub(crate) fn note_catalog_registry_change(
        &self,
        change: uqa_execution::statement::prepared::invalidation::CatalogRegistryChange,
    ) {
        change.invalidate_with_routines(
            self.session.prepared.write().values_mut(),
            &self.session.routine_bodies,
        );
        // Ordinary messages must plan again under the newly published ACL.
        if change == uqa_execution::statement::prepared::invalidation::CatalogRegistryChange::BuiltinRoutinePrivileges {
            self.session.state.write().sql_statement_cache.clear();
        }
        self.runtime.regtype_output_cache.clear();
        self.runtime.bayesian_params_cache.write().clear();
        if !self.session.transactions.lock().is_empty() {
            self.epochs
                .catalog_registry
                .dirty
                .store(true, Ordering::Release);
            return;
        }
        self.publish_catalog_registry_changes();
    }

    pub(crate) fn publish_catalog_registry_changes(&self) {
        self.runtime.bayesian_params_cache.write().clear();
        self.epochs
            .catalog_registry
            .published
            .fetch_add(1, Ordering::AcqRel);
        self.epochs
            .catalog_registry
            .dirty
            .store(false, Ordering::Release);
        self.session.state.write().sql_statement_cache.clear();
    }
}

impl Engine {
    pub(crate) fn catalog_epochs(&self) -> CatalogEpochs {
        CatalogEpochs {
            table_catalog: self.epochs.table_catalog.seen.load(Ordering::Acquire),
            table_data: self.epochs.table_data.seen.load(Ordering::Acquire),
            catalog_registry: self.epochs.catalog_registry.seen.load(Ordering::Acquire),
        }
    }

    pub(crate) fn catalog_read_view(&self) -> CatalogReadView {
        self.catalog_read_view_with_control(&uqa_core::memory::ProductionControl::uncontrolled())
            .expect("ordinary catalog capture")
    }

    pub(crate) fn restored_catalog_read_view(&self) -> CatalogReadView {
        self.restored_catalog_read_view_with_control(
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary restored catalog capture")
    }

    pub(crate) fn catalog_definition_snapshot(
        durable: &DurableCatalogSnapshot,
    ) -> CatalogDefinitionSnapshot {
        CatalogDefinitionSnapshot {
            sequence_persistence: durable.sequence_persistence.clone(),
            foreign_tables: durable.foreign_tables.clone(),
            foreign_wrappers: durable.foreign_wrappers.clone(),
            foreign_servers: durable.foreign_servers.clone(),
            sql_user_functions: durable.sql_user_functions.clone(),
            role_memberships: durable.role_memberships.clone(),

            domains: durable.domains.clone(),
            enums: durable.enums.clone(),
            ranges: durable.ranges.clone(),
            shells: durable.shells.clone(),
            comments: durable.comments.clone(),
            composites: durable.composites.clone(),
            graphs: durable.graphs.clone(),
            views: durable.views.clone(),
            catalog_indexes: durable.catalog_indexes.clone(),
            database_security: durable.database_security.clone(),
            schemas: durable.schemas.clone(),
            sequences: durable.sequences.clone(),
            sequence_object_ids: durable.sequence_object_ids.clone(),
            sequence_catalog_oids: durable.sequence_catalog_oids.clone(),
            graph_catalog_oids: durable.graph_catalog_oids.clone(),
            sequence_security: durable.sequence_security.clone(),
            foreign_table_security: durable.foreign_table_security.clone(),
            builtin_routine_security: durable.builtin_routine_security.clone(),
            system_relation_security: durable.system_relation_security.clone(),
            roles: durable.roles.clone(),
            triggers: durable.triggers.clone(),
            rules: durable.rules.clone(),
        }
    }

    pub(crate) fn catalog_read_view_from(
        &self,
        durable: &DurableCatalogSnapshot,
        table_sources: &BTreeMap<super::RelationIdentity, Arc<super::TableState>>,
    ) -> CatalogReadView {
        self.catalog_read_view_from_with_control(
            durable,
            table_sources,
            None,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary catalog generation capture")
    }

    pub(crate) fn session_execution_view(&self) -> SessionExecutionView<'_> {
        SessionExecutionView {
            session: self.session.as_ref(),
            durable: self.durable.as_ref(),
            session_id: self.session_id,
            query_transaction_origin: self.query_transaction_origin,
        }
    }

    pub(crate) fn query_runtime_view(&self) -> QueryRuntimeView<'_> {
        static EMPTY: crate::SQLFunctionContext = crate::SQLFunctionContext::empty();
        QueryRuntimeView {
            function_context: self.runtime.function_context.get().unwrap_or(&EMPTY),
            diagnostics: &self.runtime.diagnostics,
            cancellation: &self.runtime.cancellation,
            settings: self.session.as_ref(),
            scalar_functions: &self.extensions.functions.scalar,
            table_functions: &self.extensions.functions.table,
            aggregate_functions: &self.extensions.functions.aggregate,
            notices: &self.runtime.notices,
        }
    }

    pub(crate) fn mutation_coordinator(&self) -> MutationCoordinator<'_> {
        MutationCoordinator {
            storage: &self.storage,
            durable: self.durable.as_ref(),
            session: self.session.as_ref(),
            epochs: &self.epochs,
            runtime: &self.runtime,
        }
    }
}

fn default_transaction_characteristics(
    session: &SessionContext,
) -> super::TransactionCharacteristicsState {
    let isolation = match session.setting("default_transaction_isolation").as_str() {
        "read uncommitted" => TransactionIsolationLevel::ReadUncommitted,
        "repeatable read" => TransactionIsolationLevel::RepeatableRead,
        "serializable" => TransactionIsolationLevel::Serializable,
        _ => TransactionIsolationLevel::ReadCommitted,
    };
    let read_only = session.setting("default_transaction_read_only") == "on";
    let deferrable = session.setting("default_transaction_deferrable") == "on";
    super::TransactionCharacteristicsState {
        isolation,
        read_only,
        deferrable,
        assigned: 0,
    }
}

pub(crate) fn validate_stored_schema_name(name: &str) -> StorageBackendResult<()> {
    uqa_sql::schema::namespaces::validate_stored_schema_name(name)
        .map_err(StorageBackendError::Other)
}

#[cfg(test)]
mod tests;

mod catalog_execution;

pub(crate) mod session_parameters;

pub(crate) mod query_scope;

mod query_semantics;

pub(crate) mod query_expressions;
mod query_operators;
pub(crate) use query_expressions::ScopedEngineHook;
mod subqueries;

mod schema_analysis;

mod statement_effects;
mod stored_columns;
mod stored_relations;

mod partitions;

mod rule_targets;

mod query_locking;

mod table_reads;

mod view_rewrite;

mod routine_catalog;
mod routine_definitions;
mod routine_execution;
mod routine_privileges;
mod routine_removal;
mod routine_rename;
mod routine_resolution;

mod query_sources;

pub(crate) mod query_planning;

mod retrieval_catalog;

mod mutation_rows;

mod triggers;

mod constraints;

mod returning;

mod rules;

mod mutation_publication;

mod mutation_assignment;

mod referential;

pub(crate) mod query_execution;

pub(crate) mod mutation_commands;

mod insert_consumers;

mod table_creation;
mod table_functions;

mod index_creation;

mod roles;
mod sequences;

mod schema_publication;

mod hierarchy;

mod column_rewrites;

mod constraint_changes;

mod retrieval_execution;

mod physical_retrieval;

mod table_alteration;

mod column_removal;

mod copy;

mod cypher;

mod maintenance;
mod table_locks;

mod index_removal;

mod relation_removal;

mod foreign_table_alteration;
mod relation_alteration;
mod schema_rename;
mod view_alteration;
mod view_creation;
mod view_restoration;

mod comments;
mod composites;
mod domains;
mod enums;
mod namespaces;
mod range_types;
mod shell_types;

mod object_removal;
mod type_lifecycle;

mod index_routines;
mod view_dependencies;

pub(crate) mod statement_planning;

pub(crate) mod routine_invocation;

mod graph_lifecycle;

mod sequence_introspection;

mod schema_privileges;

mod database_privileges;

mod sequence_privileges;

mod table_privileges;

mod scalar_functions;

mod retrieval_planning;

mod prepared;
mod prepared_invalidation;

mod statement_transactions;

mod view_references;
mod view_removal;

pub(crate) mod truncate;

mod batch;
mod portals;
mod statements;

mod cte;
pub(crate) mod stored_routines;

mod event_definitions;

mod event_registry;

mod foreign_creation;

mod foreign_definitions;

mod foreign_catalog;

mod table_grants;

mod table_authorization;

mod table_ownership;

mod sequence_ownership;

mod sequence_dependencies;

mod sequence_removal;

mod sequence_restoration;

mod sequence_values;

mod table_removal;

impl Engine {
    pub(crate) fn catalog_read_view_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<CatalogReadView, SQLError> {
        control.check()?;
        if let Some(snapshot) = &self.query_catalog_snapshot {
            return self.with_sequence_positions_with_control(snapshot.read_view.clone(), control);
        }
        let durable = self.durable.snapshot();
        let overlay = self
            .session
            .state
            .read()
            .graph_overlay
            .as_ref()
            .map(|overlay| uqa_execution::catalog::GraphCatalogOverlay {
                names: overlay.names.clone(),
                store: overlay.store.clone(),
            });
        let tables = self.storage.tables.read();
        let view = self.catalog_read_view_from_with_control(&durable, &tables, overlay, control)?;
        drop(tables);
        self.with_sequence_positions_with_control(view, control)
    }

    pub(crate) fn restored_catalog_read_view_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<CatalogReadView, SQLError> {
        control.check()?;
        let durable = self.durable.snapshot();
        let tables = self.storage.tables.read();
        let view = self.catalog_read_view_from_with_control(&durable, &tables, None, control)?;
        drop(tables);
        self.with_sequence_positions_with_control(view, control)
    }

    fn with_sequence_positions_with_control(
        &self,
        view: CatalogReadView,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<CatalogReadView, SQLError> {
        let Some(positions) = self.shared_sequence_positions() else {
            return Ok(view);
        };
        let view = view.with_sequence_positions(positions.clone());
        Ok(match self.storage.provider.as_ref() {
            Some(provider) => view.with_latest_sequence_values(uqa_execution::catalog::sequence::latest_values::ProviderSequenceValues::shared_with_control(Arc::clone(provider), control)?),
            None => view,
        })
    }

    fn catalog_read_view_from_with_control(
        &self,
        durable: &DurableCatalogSnapshot,
        table_sources: &BTreeMap<uqa_core::RelationIdentity, Arc<crate::TableState>>,
        graph_overlay: Option<uqa_execution::catalog::GraphCatalogOverlay>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<CatalogReadView, SQLError> {
        #[cfg(test)]
        tests::record_catalog_capture();
        let mut memory = control.empty_reservation();
        let mut tables = uqa_core::memory::OwnedMap::new();
        for (relation, table) in table_sources {
            control.check()?;
            let reference = uqa_core::RelationReference {
                schema: Some(uqa_core::RelationName::decoded(&relation.schema)),
                name: uqa_core::RelationName::decoded(&relation.name),
            };
            let (relation, key_memory) = reference
                .identity_with_control(uqa_core::RelationName::decoded("public"), control)?
                .into_parts();
            memory = control.combine(memory, key_memory);
            let row_type_array_name = table
                .row_type_array_name
                .read()
                .as_ref()
                .map(|name| control.copy_text(name))
                .transpose()?;
            let row_type_array_name = row_type_array_name.map(|name| {
                let (name, name_memory) = name.into_parts();
                memory = control.combine(memory.take(), name_memory);
                name
            });
            let table = uqa_execution::catalog::CatalogTableSnapshot {
                dropped_attributes: table.dropped_attributes.snapshot(),
                object_id: table.object_id(),
                catalog_oids: table.relation_oids(),
                row_type_array_name,
                security: table.security.snapshot(),
                columns: table.columns.snapshot(),
                columns_declared: *table.columns_declared.read(),
                checks: table.table_checks.snapshot(),
                foreign_keys: table.foreign_keys.snapshot(),
                keys: table.key_constraints.snapshot(),
                hierarchy: table.hierarchy.snapshot(),
                persistence: table.persistence,
            };
            if let Some(budget) = control.budget() {
                tables.insert_budgeted(relation, table, budget)?;
            } else {
                tables.insert(relation, table);
            }
        }
        let temporary_namespace = if let Some(oids) = self.temporary_namespace_oids() {
            let (schema, schema_memory) = control
                .format(format_args!("pg_temp_{}", self.session_id))?
                .into_parts();
            memory = control.combine(memory, schema_memory);
            Some(uqa_sql::catalog::temporary_namespace::TemporaryNamespace { schema, oids })
        } else {
            None
        };
        let view = CatalogReadView::new_with_control(
            control.finish(
                uqa_execution::catalog::CatalogReadSnapshot {
                    tables,
                    definitions: Self::catalog_definition_snapshot(durable),
                    graph_overlay,
                    temporary_namespace,
                },
                memory,
            )?,
            control,
        )?;
        Ok(match self.storage.catalog.as_ref() {
            Some(catalog) => view.with_prepared_catalog(Arc::clone(catalog)),
            None => view,
        })
    }
}
