//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime services consumed by physical query operators and host callbacks.

use crate::functions::{
    RegisteredSQLFunction, SQLAggregateFunction, SQLFunctionOptions, SQLScalarFunction,
    SQLTableFunction,
};
use parking_lot::{Mutex, RwLock};
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
    pub cancellation: &'a CancellationToken,
    pub settings: &'a dyn QueryMemorySettings,
    pub scalar_functions:
        &'a RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLScalarFunction>>>,
    pub table_functions: &'a RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLTableFunction>>>,
    pub aggregate_functions:
        &'a RwLock<BTreeMap<String, RegisteredSQLFunction<dyn SQLAggregateFunction>>>,
    pub notices: &'a Mutex<Vec<(String, String)>>,
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
            .get(&name.to_ascii_lowercase())
            .cloned()
    }

    pub fn has_scalar_functions(&self) -> bool {
        !self.scalar_functions.read().is_empty()
    }

    pub fn has_scalar_function(&self, name: &str) -> bool {
        self.scalar_functions
            .read()
            .contains_key(&name.to_ascii_lowercase())
    }

    pub fn lookup_table_function(
        &self,
        name: &str,
    ) -> Option<RegisteredSQLFunction<dyn SQLTableFunction>> {
        self.table_functions
            .read()
            .get(&name.to_ascii_lowercase())
            .cloned()
    }

    pub fn has_table_function(&self, name: &str) -> bool {
        self.table_functions
            .read()
            .contains_key(&name.to_ascii_lowercase())
    }

    pub fn lookup_aggregate_function(
        &self,
        name: &str,
    ) -> Option<RegisteredSQLFunction<dyn SQLAggregateFunction>> {
        self.aggregate_functions
            .read()
            .get(&name.to_ascii_lowercase())
            .cloned()
    }

    pub fn has_aggregate_function(&self, name: &str) -> bool {
        self.aggregate_functions
            .read()
            .contains_key(&name.to_ascii_lowercase())
    }

    pub fn registered_function_options(&self, name: &str) -> [Option<SQLFunctionOptions>; 3] {
        let name = name.to_ascii_lowercase();
        [
            self.scalar_functions
                .read()
                .get(&name)
                .map(|registration| registration.options),
            self.table_functions
                .read()
                .get(&name)
                .map(|registration| registration.options),
            self.aggregate_functions
                .read()
                .get(&name)
                .map(|registration| registration.options),
        ]
    }

    pub fn push_diagnostic(&self, level: impl Into<String>, message: impl Into<String>) {
        self.notices.lock().push((level.into(), message.into()));
    }
}
