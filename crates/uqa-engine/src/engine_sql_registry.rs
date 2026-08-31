//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Arc, Engine, RegisteredSQLFunction, SQLAggregateFunction, SQLError, SQLFunctionOptions,
    SQLFunctionVolatility, SQLScalarFunction, SQLTableFunction, SQLTableFunctionResult, Value,
};

impl Engine {
    fn normalize_sql_function_name(name: &str) -> std::result::Result<String, SQLError> {
        let normalized = name.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(SQLError::TypeMismatch(
                "SQL function name cannot be empty".into(),
            ));
        }
        Ok(normalized)
    }

    pub fn register_scalar_function<F>(
        &self,
        name: &str,
        function: F,
    ) -> std::result::Result<(), SQLError>
    where
        F: SQLScalarFunction + 'static,
    {
        self.register_scalar_function_with_options(name, SQLFunctionOptions::default(), function)
    }

    /// Register a Rust scalar callback with explicit optimizer and transaction
    /// properties.
    pub fn register_scalar_function_with_options<F>(
        &self,
        name: &str,
        options: SQLFunctionOptions,
        function: F,
    ) -> std::result::Result<(), SQLError>
    where
        F: SQLScalarFunction + 'static,
    {
        Self::validate_sql_function_options(options)?;
        let name = Self::normalize_sql_function_name(name)?;
        let previous = self.extensions.scalar_functions.write().insert(
            name.clone(),
            RegisteredSQLFunction::new(Arc::new(function), options),
        );
        self.clear_sql_statement_cache();
        if let Err(error) = self.rebind_prepared_plans() {
            let mut scalars = self.extensions.scalar_functions.write();
            scalars.remove(&name);
            if let Some(previous) = previous {
                scalars.insert(name, previous);
            }
            drop(scalars);
            self.clear_sql_statement_cache();
            if let Err(cleanup) = self.rebind_prepared_plans() {
                return Err(SQLError::Internal(format!(
                    "{error}; restoring prepared plans after scalar registration failure also failed: {cleanup}"
                )));
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn register_table_function<F>(
        &self,
        name: &str,
        function: F,
    ) -> std::result::Result<(), SQLError>
    where
        F: SQLTableFunction + 'static,
    {
        self.register_table_function_with_options(name, SQLFunctionOptions::default(), function)
    }

    /// Register a Rust table callback with explicit optimizer and transaction
    /// properties.
    pub fn register_table_function_with_options<F>(
        &self,
        name: &str,
        options: SQLFunctionOptions,
        function: F,
    ) -> std::result::Result<(), SQLError>
    where
        F: SQLTableFunction + 'static,
    {
        Self::validate_sql_function_options(options)?;
        let name = Self::normalize_sql_function_name(name)?;
        let previous = self.extensions.table_functions.write().insert(
            name.clone(),
            RegisteredSQLFunction::new(Arc::new(function), options),
        );
        self.clear_sql_statement_cache();
        if let Err(error) = self.rebind_prepared_plans() {
            let mut tables = self.extensions.table_functions.write();
            tables.remove(&name);
            if let Some(previous) = previous {
                tables.insert(name, previous);
            }
            drop(tables);
            self.clear_sql_statement_cache();
            if let Err(cleanup) = self.rebind_prepared_plans() {
                return Err(SQLError::Internal(format!(
                    "{error}; restoring prepared plans after table-function registration failure also failed: {cleanup}"
                )));
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn register_aggregate_function<F>(
        &self,
        name: &str,
        function: F,
    ) -> std::result::Result<(), SQLError>
    where
        F: SQLAggregateFunction + 'static,
    {
        self.register_aggregate_function_with_options(name, SQLFunctionOptions::default(), function)
    }

    /// Register a Rust aggregate callback with explicit optimizer and
    /// transaction properties.
    pub fn register_aggregate_function_with_options<F>(
        &self,
        name: &str,
        options: SQLFunctionOptions,
        function: F,
    ) -> std::result::Result<(), SQLError>
    where
        F: SQLAggregateFunction + 'static,
    {
        Self::validate_sql_function_options(options)?;
        let name = Self::normalize_sql_function_name(name)?;
        let previous = self.extensions.aggregate_functions.write().insert(
            name.clone(),
            RegisteredSQLFunction::new(Arc::new(function), options),
        );
        // Aggregate-vs-projection is a structural choice in `QueryPlan`.
        // Cached plans compiled before this registration must be rebound.
        self.clear_sql_statement_cache();
        if let Err(error) = self.rebind_prepared_plans() {
            let mut aggregates = self.extensions.aggregate_functions.write();
            aggregates.remove(&name);
            if let Some(previous) = previous {
                aggregates.insert(name, previous);
            }
            drop(aggregates);
            self.clear_sql_statement_cache();
            if let Err(cleanup) = self.rebind_prepared_plans() {
                return Err(SQLError::Internal(format!(
                    "{error}; restoring prepared plans after aggregate registration failure also failed: {cleanup}"
                )));
            }
            return Err(error);
        }
        Ok(())
    }

    fn validate_sql_function_options(options: SQLFunctionOptions) -> Result<(), SQLError> {
        if options.may_mutate_engine && options.volatility != SQLFunctionVolatility::Volatile {
            return Err(SQLError::TypeMismatch(
                "a callback that may mutate engine state must be VOLATILE".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn call_registered_scalar_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<Value, SQLError>> {
        let registration = self.query_runtime_view().lookup_scalar_function(name)?;
        Some(registration.function.call(args))
    }

    pub(crate) fn has_registered_scalar_functions(&self) -> bool {
        self.query_runtime_view().has_scalar_functions()
    }

    pub(crate) fn has_registered_scalar_function(&self, name: &str) -> bool {
        self.query_runtime_view().has_scalar_function(name)
    }

    pub(crate) fn call_registered_table_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<SQLTableFunctionResult, SQLError>> {
        let registration = self.query_runtime_view().lookup_table_function(name)?;
        Some(registration.function.call(args))
    }

    pub(crate) fn has_registered_table_function(&self, name: &str) -> bool {
        self.query_runtime_view().has_table_function(name)
    }

    pub(crate) fn call_registered_table_function_stream(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<crate::SQLTableFunctionStream, SQLError>> {
        let registration = self.query_runtime_view().lookup_table_function(name)?;
        Some(registration.function.call_stream(args))
    }

    pub(crate) fn has_registered_aggregate_function(&self, name: &str) -> bool {
        self.query_runtime_view().has_aggregate_function(name)
    }

    pub(crate) fn registered_aggregate_function(
        &self,
        name: &str,
    ) -> Option<Arc<dyn SQLAggregateFunction>> {
        self.query_runtime_view()
            .lookup_aggregate_function(name)
            .map(|registration| registration.function)
    }

    pub(crate) fn registered_runtime_function_may_mutate_engine(&self, name: &str) -> bool {
        self.registered_runtime_function_options(name)
            .into_iter()
            .flatten()
            .any(|options| options.may_mutate_engine)
    }

    pub(crate) fn registered_runtime_function_volatility(
        &self,
        name: &str,
    ) -> Option<SQLFunctionVolatility> {
        let mut volatility = None;
        for options in self
            .registered_runtime_function_options(name)
            .into_iter()
            .flatten()
        {
            volatility = Some(match (volatility, options.volatility) {
                (_, SQLFunctionVolatility::Volatile)
                | (Some(SQLFunctionVolatility::Volatile), _) => SQLFunctionVolatility::Volatile,
                (_, SQLFunctionVolatility::Stable) | (Some(SQLFunctionVolatility::Stable), _) => {
                    SQLFunctionVolatility::Stable
                }
                _ => SQLFunctionVolatility::Immutable,
            });
        }
        volatility
    }

    fn registered_runtime_function_options(&self, name: &str) -> [Option<SQLFunctionOptions>; 3] {
        self.query_runtime_view().registered_function_options(name)
    }
}
