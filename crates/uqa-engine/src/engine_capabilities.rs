//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Narrow borrowed views over the engine's state ownership domains.

use std::sync::atomic::Ordering;

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

/// Stable catalog generations observed by one statement boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogEpochs {
    pub(crate) table_catalog: u64,
    pub(crate) table_data: u64,
    pub(crate) catalog_registry: u64,
}

/// Read-only access to catalog-owned state. This view cannot mutate transactions, acquire locks, publish caches, or recover the enclosing [`Engine`].
#[derive(Clone, Copy)]
pub(crate) struct CatalogReadView<'a> {
    storage: &'a StorageContext,
    durable: &'a DurableCatalogState,
    epochs: &'a EpochCoordinator,
    query_catalog_snapshot: Option<&'a DurableCatalogSnapshot>,
}

impl CatalogReadView<'_> {
    pub(crate) fn stable_epochs(&self) -> CatalogEpochs {
        CatalogEpochs {
            table_catalog: self.epochs.table_catalog.seen.load(Ordering::Acquire),
            table_data: self.epochs.table_data.seen.load(Ordering::Acquire),
            catalog_registry: self.epochs.catalog_registry.seen.load(Ordering::Acquire),
        }
    }

    pub(crate) fn all_schema_names(&self, session: SessionExecutionView<'_>) -> Vec<String> {
        let mut schemas = vec![
            "pg_catalog".to_string(),
            "information_schema".to_string(),
            "ag_catalog".to_string(),
        ];
        if let Some(snapshot) = self.query_catalog_snapshot {
            schemas.extend(snapshot.schemas.iter().cloned());
            schemas.extend(snapshot.graphs.keys().cloned());
        } else {
            schemas.extend(self.durable.schemas.read().iter().cloned());
            schemas.extend(self.durable.graphs.read().keys().cloned());
        }
        let temporary_schema = session.temporary_schema_name();
        let has_temporary_relation =
            self.storage
                .tables
                .read()
                .keys()
                .any(|relation| relation.schema == temporary_schema)
                || self
                    .durable
                    .views
                    .read()
                    .keys()
                    .any(|relation| relation.schema == temporary_schema)
                || self.durable.sequence_persistence.read().iter().any(
                    |(relation, persistence)| {
                        relation.schema == temporary_schema
                            && *persistence == uqa_sql::ast::RelationPersistence::Temporary
                    },
                );
        if has_temporary_relation {
            schemas.push(temporary_schema);
        }
        schemas.sort();
        schemas.dedup();
        schemas
    }

    #[cfg(test)]
    pub(crate) fn has_schema(&self, name: &str) -> bool {
        self.query_catalog_snapshot.map_or_else(
            || self.durable.schemas.read().contains(name),
            |snapshot| snapshot.schemas.contains(name),
        )
    }
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

    pub(crate) fn search_path_contains(&self, schema: &str) -> bool {
        self.session
            .state
            .read()
            .search_path
            .iter()
            .any(|candidate| candidate == schema)
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
    pub(crate) fn register_schema(
        &self,
        name: &str,
        if_not_exists: bool,
    ) -> StorageBackendResult<bool> {
        validate_schema_name(name)?;
        let mut schemas = self.durable.schemas.write();
        if schemas.contains(name) || self.durable.graphs.read().contains_key(name) {
            if if_not_exists {
                return Ok(false);
            }
            return Err(StorageBackendError::Other(format!(
                "schema `{name}` already exists"
            )));
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.save_schema(name)?;
        }
        schemas.insert(name.to_string());
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
    pub(crate) fn catalog_read_view(&self) -> CatalogReadView<'_> {
        CatalogReadView {
            storage: &self.storage,
            durable: self.durable.as_ref(),
            epochs: &self.epochs,
            query_catalog_snapshot: self.query_catalog_snapshot.as_deref(),
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
        || name.eq_ignore_ascii_case("default_transaction_isolation")
        || name.eq_ignore_ascii_case("default_transaction_read_only")
        || name.eq_ignore_ascii_case("default_transaction_deferrable")
        || name.eq_ignore_ascii_case("transaction_isolation")
        || name.eq_ignore_ascii_case("transaction_read_only")
        || name.eq_ignore_ascii_case("transaction_deferrable")
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
mod tests {
    use super::*;

    #[test]
    fn capability_views_expose_only_their_owned_state() {
        let engine = Engine::new();
        let catalog = engine.catalog_read_view();
        let session = engine.session_execution_view();
        let runtime = engine.query_runtime_view();
        assert!(catalog.has_schema("public"));
        assert_eq!(session.search_path(), vec!["public"]);
        assert_eq!(session.current_user(), "uqa");
        assert_eq!(session.session_user(), "uqa");
        assert_eq!(session.transaction_depth(), 0);
        assert_eq!(session.transaction_snapshot_identity(), None);
        assert_eq!(runtime.work_mem_bytes().unwrap(), 64 * 1024 * 1024);
        runtime.check_cancelled().unwrap();
    }

    #[test]
    fn mutation_coordinator_publishes_schema_changes_without_engine_recovery() {
        let engine = Engine::new();
        let before = engine.catalog_read_view().stable_epochs();
        assert!(engine
            .mutation_coordinator()
            .register_schema("capability_test", false)
            .unwrap());
        assert!(engine.catalog_read_view().has_schema("capability_test"));
        let after = engine.catalog_read_view().stable_epochs();
        assert_eq!(after.catalog_registry, before.catalog_registry);
        assert!(
            engine
                .epochs
                .catalog_registry
                .published
                .load(Ordering::Acquire)
                > before.catalog_registry
        );
    }
}
