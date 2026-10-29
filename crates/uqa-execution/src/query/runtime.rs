//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime services consumed by physical query operators and host callbacks.

use crate::functions::{
    RegisteredSQLFunction, SQLAggregateFactory, SQLFunctionContext, SQLFunctionOptions,
    SQLScalarFunction, SQLTableFunction,
};
use parking_lot::RwLock;
use std::collections::BTreeMap;
use uqa_core::CancellationToken;
use uqa_sql::SQLError;

/// Live memory settings are read at the same execution boundaries as the session setting.
pub trait QueryMemorySettings: Send + Sync {
    fn work_mem_bytes(&self) -> Result<usize, SQLError>;
}

/// Borrowed runtime services with no catalog mutation, transaction, or storage publication access.
#[derive(Clone, Copy)]
pub struct QueryRuntimeView<'a> {
    pub diagnostics: &'a super::diagnostics::QueryDiagnostics,
    pub cancellation: &'a CancellationToken,
    pub function_context: &'a SQLFunctionContext,
    pub settings: &'a dyn QueryMemorySettings,
    pub scalar_functions:
        &'a RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLScalarFunction>>>,
    pub table_functions: &'a RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLTableFunction>>>,
    pub aggregate_functions:
        &'a RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLAggregateFactory>>>,
    pub notices: &'a super::NoticeQueue,
}

impl QueryRuntimeView<'_> {
    pub fn check_cancelled(&self) -> Result<(), SQLError> {
        Ok(self.cancellation.check()?)
    }

    pub fn cancellation_token(&self) -> uqa_core::CancellationToken {
        self.cancellation.clone()
    }

    pub fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        self.settings.work_mem_bytes()
    }

    pub fn lookup_scalar_function(
        &self,
        name: &str,
    ) -> Option<RegisteredSQLFunction<dyn SQLScalarFunction>> {
        self.scalar_functions
            .read()
            .get(runtime_function_name(name).as_str())
            .cloned()
    }

    pub fn has_scalar_functions(&self) -> bool {
        !self.scalar_functions.read().is_empty()
    }

    pub fn has_scalar_function(&self, name: &str) -> bool {
        self.scalar_functions
            .read()
            .contains_key(runtime_function_name(name).as_str())
    }

    pub fn has_scalar_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let name = uqa_sql::semantics::lowercase_function_name_with_control(name, control)?;
        control.check()?;
        Ok(self.scalar_functions.read().contains_key(name.as_str()))
    }

    pub fn lookup_table_function(
        &self,
        name: &str,
    ) -> Option<RegisteredSQLFunction<dyn SQLTableFunction>> {
        self.table_functions
            .read()
            .get(runtime_function_name(name).as_str())
            .cloned()
    }

    pub fn has_table_function(&self, name: &str) -> bool {
        self.table_functions
            .read()
            .contains_key(runtime_function_name(name).as_str())
    }

    pub fn lookup_aggregate_function(
        &self,
        name: &str,
    ) -> Option<RegisteredSQLFunction<dyn SQLAggregateFactory>> {
        self.aggregate_functions
            .read()
            .get(runtime_function_name(name).as_str())
            .cloned()
    }

    pub fn has_aggregate_function(&self, name: &str) -> bool {
        self.aggregate_functions
            .read()
            .contains_key(runtime_function_name(name).as_str())
    }

    pub fn registered_function_options(&self, name: &str) -> [Option<SQLFunctionOptions>; 3] {
        let name = runtime_function_name(name);
        [
            self.scalar_functions
                .read()
                .get(name.as_str())
                .map(|registration| registration.options),
            self.table_functions
                .read()
                .get(name.as_str())
                .map(|registration| registration.options),
            self.aggregate_functions
                .read()
                .get(name.as_str())
                .map(|registration| registration.options),
        ]
    }

    pub fn has_aggregate_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let name = uqa_sql::semantics::lowercase_function_name_with_control(name, control)?;
        control.check()?;
        Ok(self.aggregate_functions.read().contains_key(name.as_str()))
    }

    pub fn has_table_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let name = uqa_sql::semantics::lowercase_function_name_with_control(name, control)?;
        control.check()?;
        Ok(self.table_functions.read().contains_key(name.as_str()))
    }

    /// Queue a notice for the client, after the notices queued before it.
    pub fn push_notice(&self, notice: uqa_sql::SQLNotice) {
        self.notices.push(notice);
    }
}

fn runtime_function_name(name: &str) -> uqa_sql::semantics::FunctionName<'_> {
    uqa_sql::semantics::lowercase_function_name_with_control(
        name,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary runtime identity normalization")
}
