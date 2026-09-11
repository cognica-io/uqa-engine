//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Narrow capabilities over the engine's state ownership domains.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

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

use uqa_execution::catalog::{CatalogDefinitionSnapshot, CatalogReadSnapshot};
pub(crate) use uqa_execution::catalog::{
    CatalogReadView, CatalogTableSnapshot, RelationResolution,
};

/// Read-only session values visible to statement execution. Durable registries and storage backends are intentionally absent.
#[derive(Clone, Copy)]
pub(crate) struct SessionExecutionView<'a> {
    session: &'a SessionContext,
    session_id: u64,
    query_transaction_origin: Option<u64>,
}

impl SessionExecutionView<'_> {
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

    pub(crate) fn search_path(&self) -> Vec<String> {
        self.session.state.read().search_path.clone()
    }

    pub(crate) fn current_user(&self) -> String {
        self.session.state.read().current_user.clone()
    }

    pub(crate) fn session_user(&self) -> String {
        self.session.state.read().session_user.clone()
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
            search_path: state.search_path.clone(),
            temporary_schema: self.temporary_schema_name(),
            temporary_namespace_allocated: state.temporary_namespace_allocated,
            current_user: state.current_user.clone(),
            lookup_mode: RelationLookupMode::Dynamic,
        }
    }

    pub(crate) fn show_variable(&self, name: &str) -> Result<String, SQLError> {
        if name.eq_ignore_ascii_case("search_path") {
            return Ok(self.search_path().join(","));
        }
        if let Some(value) = self.transaction_parameter_value(name) {
            return Ok(value);
        }
        let session = self.session.state.read();
        if let Some(value) = session_value(&session.session_vars, name) {
            return Ok(value);
        }
        default_runtime_parameter(name)
            .map(str::to_string)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("unrecognized configuration parameter \"{name}\""),
            })
    }

    pub(crate) fn runtime_parameter_source(&self, name: &str) -> &'static str {
        if self
            .session
            .state
            .read()
            .session_vars
            .keys()
            .any(|key| key.eq_ignore_ascii_case(name))
        {
            "session"
        } else {
            "default"
        }
    }

    fn transaction_parameter_value(&self, name: &str) -> Option<String> {
        let current = self.session.transactions.lock().last().map_or_else(
            || default_transaction_characteristics(self.session),
            |frame| frame.characteristics,
        );
        if name.eq_ignore_ascii_case("transaction_isolation") {
            return Some(current.isolation.as_str().into());
        }
        if name.eq_ignore_ascii_case("transaction_read_only") {
            return Some(if current.read_only { "on" } else { "off" }.into());
        }
        if name.eq_ignore_ascii_case("transaction_deferrable") {
            return Some(if current.deferrable { "on" } else { "off" }.into());
        }
        None
    }
}

pub(crate) use uqa_execution::query::runtime::QueryRuntimeView;

impl uqa_execution::query::runtime::QueryMemorySettings for SessionContext {
    fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        let session = self.state.read();
        let setting = session_value_ref(&session.session_vars, "work_mem")
            .unwrap_or_else(|| default_runtime_parameter("work_mem").unwrap());
        parse_work_mem_bytes(setting)
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

    pub(crate) fn end_command_mutation_overlay(&self) {
        let removed = self.session.command_mutation_overlays.lock().pop();
        debug_assert!(
            removed.is_some(),
            "command mutation overlay stack underflow"
        );
    }

    pub(crate) fn register_schema(
        &self,
        name: &str,
        if_not_exists: bool,
        role_owner: &str,
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
        )
    }

    pub(crate) fn note_catalog_registry_changed(&self) {
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
        let durable = self.query_catalog_snapshot.clone().unwrap_or_else(|| {
            let mut snapshot = self.durable.snapshot();
            snapshot.graphs = self.visible_graph_handles();
            Arc::new(snapshot)
        });
        let table_sources = self.query_table_snapshots.as_ref().map_or_else(
            || self.storage.tables.read().clone(),
            |tables| (**tables).clone(),
        );
        Self::catalog_read_view_from(&durable, table_sources)
    }

    pub(crate) fn restored_catalog_read_view(&self) -> CatalogReadView {
        Self::catalog_read_view_from(&self.durable.snapshot(), self.storage.tables.read().clone())
    }

    fn catalog_read_view_from(
        durable: &DurableCatalogSnapshot,
        table_sources: BTreeMap<super::RelationIdentity, Arc<super::TableState>>,
    ) -> CatalogReadView {
        let tables = table_sources
            .into_iter()
            .map(|(relation, table)| {
                let snapshot = CatalogTableSnapshot {
                    object_id: table.object_id(),
                    security: table.security.snapshot(),
                    columns: table.columns.snapshot(),
                    columns_declared: *table.columns_declared.read(),
                    checks: table.table_checks.snapshot(),
                    foreign_keys: table.foreign_keys.snapshot(),
                    keys: table.key_constraints.snapshot(),
                    hierarchy: table.hierarchy.snapshot(),
                    persistence: table.persistence,
                };
                (relation, snapshot)
            })
            .collect();
        CatalogReadView::new(CatalogReadSnapshot {
            tables,
            definitions: CatalogDefinitionSnapshot {
                sequence_persistence: durable.sequence_persistence.clone(),
                foreign_tables: durable.foreign_tables.clone(),
                sql_user_functions: durable.sql_user_functions.clone(),
                role_memberships: durable.role_memberships.clone(),

                domains: durable.domains.clone(),
                graphs: durable.graphs.clone(),
                views: durable.views.clone(),
                catalog_indexes: durable.catalog_indexes.clone(),
                database_security: durable.database_security.clone(),
                schemas: durable.schemas.clone(),
                sequences: durable.sequences.clone(),
                sequence_object_ids: durable.sequence_object_ids.clone(),
                sequence_security: durable.sequence_security.clone(),
                foreign_table_security: durable.foreign_table_security.clone(),
                roles: durable.roles.clone(),
                triggers: durable.triggers.clone(),
                rules: durable.rules.clone(),
            },
        })
    }

    pub(crate) fn session_execution_view(&self) -> SessionExecutionView<'_> {
        SessionExecutionView {
            session: self.session.as_ref(),
            session_id: self.session_id,
            query_transaction_origin: self.query_transaction_origin,
        }
    }

    pub(crate) fn query_runtime_view(&self) -> QueryRuntimeView<'_> {
        QueryRuntimeView {
            cancellation: &self.runtime.cancellation,
            settings: self.session.as_ref(),
            scalar_functions: &self.extensions.scalar_functions,
            table_functions: &self.extensions.table_functions,
            aggregate_functions: &self.extensions.aggregate_functions,
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

pub(super) fn default_runtime_parameter(name: &str) -> Option<&'static str> {
    if name.eq_ignore_ascii_case("application_name") {
        return Some("");
    }
    if name.eq_ignore_ascii_case("standard_conforming_strings")
        || name.eq_ignore_ascii_case("integer_datetimes")
    {
        return Some("on");
    }
    if name.eq_ignore_ascii_case("server_version_num") {
        return Some("180000");
    }
    if name.eq_ignore_ascii_case("server_version") {
        return Some("18.0-uqa");
    }
    if name.eq_ignore_ascii_case("server_encoding") || name.eq_ignore_ascii_case("client_encoding")
    {
        return Some("UTF8");
    }
    if name.eq_ignore_ascii_case("datestyle") {
        return Some("ISO, MDY");
    }
    if name.eq_ignore_ascii_case("timezone") {
        return Some("UTC");
    }
    if name.eq_ignore_ascii_case("work_mem") {
        return Some("64MB");
    }
    if name.eq_ignore_ascii_case("plan_cache_mode") {
        return Some("auto");
    }
    if name.eq_ignore_ascii_case("session_replication_role") {
        return Some("origin");
    }
    if name.eq_ignore_ascii_case("plpgsql.check_asserts") {
        return Some("on");
    }
    if name.eq_ignore_ascii_case("default_transaction_isolation")
        || name.eq_ignore_ascii_case("transaction_isolation")
    {
        return Some("read committed");
    }
    if name.eq_ignore_ascii_case("default_transaction_read_only")
        || name.eq_ignore_ascii_case("default_transaction_deferrable")
        || name.eq_ignore_ascii_case("transaction_read_only")
        || name.eq_ignore_ascii_case("transaction_deferrable")
    {
        return Some("off");
    }
    None
}

pub(super) fn is_known_runtime_parameter(name: &str) -> bool {
    name.eq_ignore_ascii_case("search_path") || default_runtime_parameter(name).is_some()
}

pub(super) fn is_mutable_runtime_parameter(name: &str) -> bool {
    name.eq_ignore_ascii_case("application_name")
        || name.eq_ignore_ascii_case("search_path")
        || name.eq_ignore_ascii_case("client_encoding")
        || name.eq_ignore_ascii_case("datestyle")
        || name.eq_ignore_ascii_case("timezone")
        || name.eq_ignore_ascii_case("work_mem")
        || name.eq_ignore_ascii_case("plan_cache_mode")
        || name.eq_ignore_ascii_case("session_replication_role")
        || name.eq_ignore_ascii_case("plpgsql.check_asserts")
        || name.eq_ignore_ascii_case("default_transaction_isolation")
        || name.eq_ignore_ascii_case("default_transaction_read_only")
        || name.eq_ignore_ascii_case("default_transaction_deferrable")
        || name.eq_ignore_ascii_case("transaction_isolation")
        || name.eq_ignore_ascii_case("transaction_read_only")
        || name.eq_ignore_ascii_case("transaction_deferrable")
}

pub(super) fn parse_boolean_runtime_parameter(name: &str, value: &str) -> Result<bool, SQLError> {
    let text = value.trim().to_ascii_lowercase();
    let matches_prefix = |word: &str| !text.is_empty() && word.starts_with(&text);
    if matches_prefix("true") || matches_prefix("yes") || text == "on" || text == "1" {
        return Ok(true);
    }
    if matches_prefix("false")
        || matches_prefix("no")
        || (matches_prefix("off") && text.len() >= 2)
        || text == "0"
    {
        return Ok(false);
    }
    Err(SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("parameter \"{name}\" requires a Boolean value"),
    })
}

fn session_value(
    values: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> Option<String> {
    session_value_ref(values, name).map(str::to_string)
}

fn session_value_ref<'a>(
    values: &'a std::collections::BTreeMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    values.get(name).map(String::as_str).or_else(|| {
        values
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    })
}

fn default_transaction_characteristics(
    session: &SessionContext,
) -> super::TransactionCharacteristicsState {
    let state = session.state.read();
    let isolation =
        match session_value(&state.session_vars, "default_transaction_isolation").as_deref() {
            Some("read uncommitted") => TransactionIsolationLevel::ReadUncommitted,
            Some("repeatable read") => TransactionIsolationLevel::RepeatableRead,
            Some("serializable") => TransactionIsolationLevel::Serializable,
            _ => TransactionIsolationLevel::ReadCommitted,
        };
    let read_only = session_value(&state.session_vars, "default_transaction_read_only")
        .is_some_and(|value| value == "on");
    let deferrable = session_value(&state.session_vars, "default_transaction_deferrable")
        .is_some_and(|value| value == "on");
    super::TransactionCharacteristicsState {
        isolation,
        read_only,
        deferrable,
    }
}

pub(super) fn parse_work_mem_bytes(raw: &str) -> Result<usize, SQLError> {
    let compact = raw
        .trim()
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    let digits = compact.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return Err(SQLError::TypeMismatch(format!(
            "work_mem must be a positive byte size, got {raw:?}"
        )));
    }
    let amount = compact[..digits].parse::<usize>().map_err(|_| {
        SQLError::TypeMismatch(format!("work_mem is outside the supported range: {raw:?}"))
    })?;
    if amount == 0 {
        return Err(SQLError::TypeMismatch(
            "work_mem must be greater than zero".into(),
        ));
    }
    let unit = compact[digits..].to_ascii_lowercase();
    let exponent = match unit.as_str() {
        "b" => 0,
        "" | "k" | "kb" | "kib" => 1,
        "m" | "mb" | "mib" => 2,
        "g" | "gb" | "gib" => 3,
        "t" | "tb" | "tib" => 4,
        _ => {
            return Err(SQLError::TypeMismatch(format!(
                "unsupported work_mem unit in {raw:?}"
            )))
        }
    };
    let multiplier = 1024_usize.checked_pow(exponent).ok_or_else(|| {
        SQLError::TypeMismatch(format!("work_mem is outside the supported range: {raw:?}"))
    })?;
    amount.checked_mul(multiplier).ok_or_else(|| {
        SQLError::TypeMismatch(format!("work_mem is outside the supported range: {raw:?}"))
    })
}

pub(crate) fn validate_schema_name(name: &str) -> StorageBackendResult<()> {
    uqa_sql::schema::namespaces::validate_schema_name(name).map_err(StorageBackendError::Other)
}

#[cfg(test)]
mod tests;

mod catalog_execution;

pub(crate) mod query_scope;

mod query_semantics;

mod query_operators;

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

mod index_creation;

mod sequences;

mod schema_publication;
pub(crate) use schema_publication::allocate_catalog_object_id;

mod hierarchy;

mod column_rewrites;

mod constraint_changes;

mod retrieval_execution;

mod physical_retrieval;

mod table_alteration;

mod column_removal;

mod copy;

mod maintenance;

mod index_removal;

mod relation_removal;

mod foreign_table_alteration;
mod relation_alteration;
mod view_alteration;

mod domains;
mod namespaces;

mod index_routines;
mod view_dependencies;
