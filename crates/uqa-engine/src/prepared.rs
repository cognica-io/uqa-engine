//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Engine;
use crate::transactions::prepared::PreparedOperation;
use std::sync::{Arc, Weak};
use uqa_sql::{ColumnType, RowSchema, SQLError, SQLParam, SQLResult};

/// A session-owned prepared definition's identity. Removing and recreating its catalog name never rebinds this handle.
pub struct PreparedStatement {
    name: String,
    source: Arc<uqa_sql::Statement>,
    owner: Weak<crate::state::SessionContext>,
    definition: Weak<uqa_sql::plan::UnifiedPlan>,
}

/// Preparation-time parameter and positional result descriptors.
pub struct PreparedStatementDescription {
    pub parameter_types: Vec<Option<ColumnType>>,
    pub result_schema: Option<RowSchema>,
}

impl PreparedStatement {
    fn matches(&self, engine: &Engine, entry: &super::PreparedStatementPlan) -> bool {
        self.owner.ptr_eq(&Arc::downgrade(&engine.session))
            && self.definition.ptr_eq(&Arc::downgrade(&entry.source_plan))
    }

    fn invalid(&self) -> SQLError {
        uqa_sql::prepared::statement_error("26000", &self.name, "does not exist")
    }
}

impl Engine {
    /// Prepare native SQL with optional explicit parameter declarations. Definitions survive transaction rollback and are removed by finalize, DEALLOCATE or DISCARD ALL.
    pub fn prepare_statement(
        &self,
        sql: &str,
        declared: &[ColumnType],
    ) -> Result<PreparedStatement, SQLError> {
        self.with_prepared_registration(
            |engine| {
                uqa_execution::statement::prepared::parse_host_sql(
                    sql,
                    engine.parser_settings(),
                    &engine.runtime.notices,
                    &engine.runtime.cancellation,
                )
            },
            |engine, source| {
                let name = loop {
                    let identity = engine
                        .session
                        .next_prepared_handle
                        .fetch_update(
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                            |value| value.checked_add(1),
                        )
                        .map_err(|_| SQLError::Routine {
                            sqlstate: "54000".into(),
                            message: "prepared statement identity space exhausted".into(),
                        })?;
                    let name = format!("__uqa_prepared_{identity}");
                    if !engine.session.prepared.read().contains_key(&name) {
                        break name;
                    }
                };
                uqa_execution::statement::prepared::register_host_statement(
                    &engine.prepared_registration_context(),
                    name.clone(),
                    &source,
                    sql,
                    declared,
                )?;
                let registry = engine.session.prepared.read();
                let entry = registry.get(&name).expect("published prepared definition");
                Ok(PreparedStatement {
                    name,
                    source,
                    owner: Arc::downgrade(&engine.session),
                    definition: Arc::downgrade(&entry.source_plan),
                })
            },
        )
    }

    pub fn validate_prepared_statement(
        &self,
        statement: &PreparedStatement,
    ) -> Result<(), SQLError> {
        let _gate = self.lock_statement_gate();
        if self
            .session
            .prepared
            .read()
            .get(&statement.name)
            .is_some_and(|entry| statement.matches(self, entry))
        {
            Ok(())
        } else {
            Err(statement.invalid())
        }
    }

    pub fn describe_prepared_statement(
        &self,
        statement: &PreparedStatement,
    ) -> Result<PreparedStatementDescription, SQLError> {
        self.run_prepared_operation(
            || {
                let registry = self.session.prepared.read();
                let entry = registry
                    .get(&statement.name)
                    .filter(|entry| statement.matches(self, entry))
                    .ok_or_else(|| statement.invalid())?;
                Ok((
                    PreparedOperation::Describe {
                        returns_rows: entry.result_schema.is_some(),
                    },
                    PreparedStatementDescription {
                        parameter_types: entry.parameter_types.clone(),
                        result_schema: entry.result_schema.clone(),
                    },
                ))
            },
            Ok,
        )
    }

    pub fn execute_prepared_statement(
        &self,
        statement: &PreparedStatement,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError> {
        self.run_prepared_operation(
            || {
                let registry = self.session.prepared.read();
                let entry = registry
                    .get(&statement.name)
                    .filter(|entry| statement.matches(self, entry))
                    .ok_or_else(|| statement.invalid())?;
                uqa_sql::prepared::validate_host_parameter_count(
                    &statement.name,
                    params.len(),
                    entry.parameter_types.len(),
                )?;
                Ok((
                    PreparedOperation::Bind {
                        statement: Arc::clone(&statement.source),
                        parameter_count: params.len(),
                    },
                    entry.source_sql.clone(),
                ))
            },
            |sql| {
                self.execute_compiled_prepared(
                    &statement.name,
                    Arc::clone(&statement.source),
                    sql.as_deref(),
                    params,
                )
            },
        )
    }

    /// An invalidated handle is harmless to release and cannot remove a replacement definition.
    pub fn finalize_prepared_statement(&self, statement: &PreparedStatement) {
        let _gate = self.lock_statement_gate();
        let mut registry = self.session.prepared.write();
        if registry
            .get(&statement.name)
            .is_some_and(|entry| statement.matches(self, entry))
        {
            registry.remove(&statement.name);
        }
    }

    pub fn register_prepared(
        &self,
        name: String,
        definition: uqa_sql::ast::Statement,
    ) -> Result<(), uqa_sql::SQLError> {
        self.with_prepared_registration(
            |_| Ok(Arc::new(definition)),
            |engine, definition| {
                uqa_execution::statement::prepared::register_statement(
                    &engine.prepared_registration_context(),
                    name,
                    definition.as_ref().clone(),
                )
            },
        )
    }

    pub(crate) fn prepared_parameter_types(
        &self,
        name: &str,
    ) -> Option<Vec<Option<uqa_sql::ast::ColumnType>>> {
        self.session
            .prepared
            .read()
            .get(name)
            .map(|entry| entry.parameter_types.clone())
    }

    pub fn lookup_prepared(&self, name: &str) -> Option<uqa_planner::UnifiedPlan> {
        self.session
            .prepared
            .read()
            .get(name)
            .map(|entry| entry.plan.as_ref().unwrap_or(&entry.logical_plan).clone())
    }

    /// Invalidate executable plans without removing connection-owned definitions.
    /// Catalog changes are checked when each statement is next executed, so a
    /// dropped dependency cannot make unrelated commands or rollback fail.
    pub(crate) fn invalidate_prepared_plans(&self) {
        uqa_execution::statement::prepared::invalidation::invalidate_execution_plans_with_routines(
            self.session.prepared.write().values_mut(),
            &self.session.routine_bodies,
        );
    }

    /// Read original input syntax again after a binding dependency changes.
    pub(crate) fn invalidate_prepared_analysis(&self) {
        uqa_execution::statement::prepared::invalidation::PreparedCatalogChange::GlobalCatalog
            .invalidate_with_routines(
                self.session.prepared.write().values_mut(),
                &self.session.routine_bodies,
            );
    }

    pub fn deallocate_prepared(&self, name: Option<&str>) {
        let _gate = self.lock_statement_gate();
        match name {
            Some(name) => {
                self.session.prepared.write().remove(name);
            }
            None => self.session.prepared.write().clear(),
        }
    }
}
