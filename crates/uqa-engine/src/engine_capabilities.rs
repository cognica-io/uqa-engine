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

use super::engine_session::is_virtual_system_schema;
use super::engine_state::{
    DurableCatalogSnapshot, DurableCatalogState, EpochCoordinator, QueryRuntime, RuntimeExtensions,
    SessionContext, StorageContext,
};
use super::{
    Engine, RegisteredSQLFunction, SQLAggregateFunction, SQLScalarFunction, SQLTableFunction,
};

mod catalog;

/// Stable catalog generations observed by one statement boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogEpochs {
    pub(crate) table_catalog: u64,
    pub(crate) table_data: u64,
    pub(crate) catalog_registry: u64,
}

/// Read-only access to catalog-owned state. This view cannot mutate transactions, acquire locks, publish caches, or recover the enclosing [`crate::Engine`].
#[derive(Clone)]
pub(crate) struct CatalogReadView {
    pub(super) snapshot: Arc<CatalogReadSnapshot>,
}

/// Immutable catalog names and durable registries captured at one statement boundary.
#[derive(Clone)]
pub(crate) struct CatalogReadSnapshot {
    pub(super) tables: BTreeMap<crate::RelationIdentity, CatalogTableSnapshot>,
    pub(super) durable: Arc<DurableCatalogSnapshot>,
}

/// Immutable table-definition fields used by binding and catalog projection.
#[derive(Clone)]
pub(crate) struct CatalogTableSnapshot {
    pub(crate) object_id: [u8; 16],
    pub(crate) columns: Vec<uqa_sql::ast::ColumnDef>,
    pub(crate) checks: Vec<uqa_sql::ast::TableCheck>,
    pub(crate) foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
    pub(crate) keys: Vec<uqa_sql::ast::TableKeyConstraint>,
    pub(crate) hierarchy: uqa_sql::ast::TableHierarchy,
    pub(crate) persistence: uqa_sql::ast::RelationPersistence,
}

/// Immutable state for one sequence selected through the statement's relation namespace.
#[derive(Clone)]
pub(crate) struct CatalogSequenceSnapshot {
    pub(crate) relation: crate::RelationIdentity,
    pub(crate) state: crate::SequenceState,
    pub(crate) security: super::engine_state::SequenceSecurity,
}

/// Immutable session inputs used to resolve unqualified relation names during one statement.
#[derive(Clone)]
pub(crate) struct RelationNameResolution {
    pub(super) search_path: Vec<String>,
    pub(super) temporary_schema: String,
    pub(super) current_user: String,
    pub(super) lookup_mode: RelationLookupMode,
}

/// Whether a query resolves session-visible names or follows catalog identities captured when a stored expression was defined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RelationLookupMode {
    Dynamic,
    Bound,
}

/// Read-only session values visible to statement execution. Durable registries and storage backends are intentionally absent.
#[derive(Clone, Copy)]
pub(crate) struct SessionExecutionView<'a> {
    session: &'a SessionContext,
    session_id: u64,
    query_transaction_origin: Option<u64>,
}

impl SessionExecutionView<'_> {
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
        RelationNameResolution {
            search_path: self.search_path(),
            temporary_schema: self.temporary_schema_name(),
            current_user: self.current_user(),
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

/// Runtime-only query services. The view owns no catalog mutation, transaction-stack, or storage-publication capability.
#[derive(Clone, Copy)]
pub(crate) struct QueryRuntimeView<'a> {
    runtime: &'a QueryRuntime,
    extensions: &'a RuntimeExtensions,
    session_state: &'a parking_lot::RwLock<super::SessionStateSnapshot>,
}

impl QueryRuntimeView<'_> {
    pub(crate) fn check_cancelled(&self) -> Result<(), SQLError> {
        Ok(self.runtime.cancellation.check()?)
    }

    pub(crate) fn cancellation_token(&self) -> uqa_core::CancellationToken {
        self.runtime.cancellation.clone()
    }

    pub(crate) fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        let session = self.session_state.read();
        let setting = session_value_ref(&session.session_vars, "work_mem")
            .unwrap_or_else(|| default_runtime_parameter("work_mem").unwrap());
        parse_work_mem_bytes(setting)
    }

    pub(crate) fn lookup_scalar_function(
        &self,
        name: &str,
    ) -> Option<RegisteredSQLFunction<dyn SQLScalarFunction>> {
        self.extensions
            .scalar_functions
            .read()
            .get(&name.to_ascii_lowercase())
            .cloned()
    }

    pub(crate) fn has_scalar_functions(&self) -> bool {
        !self.extensions.scalar_functions.read().is_empty()
    }

    pub(crate) fn has_scalar_function(&self, name: &str) -> bool {
        self.extensions
            .scalar_functions
            .read()
            .contains_key(&name.to_ascii_lowercase())
    }

    pub(crate) fn lookup_table_function(
        &self,
        name: &str,
    ) -> Option<RegisteredSQLFunction<dyn SQLTableFunction>> {
        self.extensions
            .table_functions
            .read()
            .get(&name.to_ascii_lowercase())
            .cloned()
    }

    pub(crate) fn has_table_function(&self, name: &str) -> bool {
        self.extensions
            .table_functions
            .read()
            .contains_key(&name.to_ascii_lowercase())
    }

    pub(crate) fn lookup_aggregate_function(
        &self,
        name: &str,
    ) -> Option<RegisteredSQLFunction<dyn SQLAggregateFunction>> {
        self.extensions
            .aggregate_functions
            .read()
            .get(&name.to_ascii_lowercase())
            .cloned()
    }

    pub(crate) fn has_aggregate_function(&self, name: &str) -> bool {
        self.extensions
            .aggregate_functions
            .read()
            .contains_key(&name.to_ascii_lowercase())
    }

    pub(crate) fn registered_function_options(
        &self,
        name: &str,
    ) -> [Option<super::SQLFunctionOptions>; 3] {
        let name = name.to_ascii_lowercase();
        [
            self.extensions
                .scalar_functions
                .read()
                .get(&name)
                .map(|registration| registration.options),
            self.extensions
                .table_functions
                .read()
                .get(&name)
                .map(|registration| registration.options),
            self.extensions
                .aggregate_functions
                .read()
                .get(&name)
                .map(|registration| registration.options),
        ]
    }

    pub(crate) fn push_diagnostic(&self, level: impl Into<String>, message: impl Into<String>) {
        self.runtime
            .notices
            .lock()
            .push((level.into(), message.into()));
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
        validate_schema_name(name)?;
        let mut schemas = self.durable.schemas.write();
        if schemas.contains_key(name) || self.durable.graphs.read().contains_key(name) {
            if if_not_exists {
                return Ok(false);
            }
            return Err(StorageBackendError::Other(format!(
                "schema `{name}` already exists"
            )));
        }
        let security = super::engine_state::SchemaSecurity {
            role_owner: role_owner.to_string(),
            acl: None,
        };
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.save_schema_row(&security.row(name))?;
        }
        schemas.insert(name.to_string(), security);
        drop(schemas);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    pub(crate) fn note_catalog_registry_changed(&self) {
        self.runtime
            .regtype_output_cache_revision
            .fetch_add(1, Ordering::AcqRel);
        *self.runtime.regtype_output_cache.lock() = None;
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
        let durable = self
            .query_catalog_snapshot
            .clone()
            .unwrap_or_else(|| Arc::new(self.durable.snapshot()));
        let table_sources = self.query_table_snapshots.as_ref().map_or_else(
            || self.storage.tables.read().clone(),
            |tables| (**tables).clone(),
        );
        Self::catalog_read_view_from(durable, table_sources)
    }

    pub(crate) fn restored_catalog_read_view(&self) -> CatalogReadView {
        Self::catalog_read_view_from(
            Arc::new(self.durable.snapshot()),
            self.storage.tables.read().clone(),
        )
    }

    fn catalog_read_view_from(
        durable: Arc<DurableCatalogSnapshot>,
        table_sources: BTreeMap<super::RelationIdentity, Arc<super::TableState>>,
    ) -> CatalogReadView {
        let tables = table_sources
            .into_iter()
            .map(|(relation, table)| {
                let snapshot = CatalogTableSnapshot {
                    object_id: table.object_id(),
                    columns: table.columns.read().clone(),
                    checks: table.table_checks.read().clone(),
                    foreign_keys: table.foreign_keys.read().clone(),
                    keys: table.key_constraints.read().clone(),
                    hierarchy: table.hierarchy.read().clone(),
                    persistence: table.persistence,
                };
                (relation, snapshot)
            })
            .collect();
        CatalogReadView {
            snapshot: Arc::new(CatalogReadSnapshot { tables, durable }),
        }
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
            runtime: &self.runtime,
            extensions: &self.extensions,
            session_state: &self.session.state,
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
    name.eq_ignore_ascii_case("search_path")
        || name.eq_ignore_ascii_case("client_encoding")
        || name.eq_ignore_ascii_case("datestyle")
        || name.eq_ignore_ascii_case("timezone")
        || name.eq_ignore_ascii_case("work_mem")
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
    if name.is_empty() {
        return Err(StorageBackendError::Other(format!(
            "invalid schema name `{name}`"
        )));
    }
    if is_virtual_system_schema(name) {
        return Err(StorageBackendError::Other(format!(
            "schema name `{name}` is reserved"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
