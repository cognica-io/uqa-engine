//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Arc, Engine, RegisteredSQLFunction, SQLAggregateFactory, SQLError, SQLFunctionOptions,
    SQLFunctionVolatility, SQLScalarFunction, SQLTableFunction, Value,
};

mod owner;
mod retained;
pub(crate) use owner::Registry;
pub use owner::{SQLFunctionRegistration, SQLFunctionRegistrationError};

impl Engine {
    /// Install immutable host invocation data for this session. Public sibling
    /// sessions start empty; internal readers and portal workers inherit it.
    pub fn install_sql_function_context(
        &self,
        context: crate::SQLFunctionContext,
    ) -> Result<(), SQLFunctionRegistrationError> {
        let _gate = self
            .runtime
            .statement_gate
            .try_lock_idle()
            .ok_or(SQLFunctionRegistrationError::Busy)?;
        self.runtime
            .function_context
            .set(context)
            .map_err(|_| SQLError::Routine {
                sqlstate: "55000".into(),
                message: "SQL function context is already installed for this session".into(),
            })?;
        Ok(())
    }

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
        self.register_scalar_function_retained(name, options, function)
            .map(drop)
            .map_err(Into::into)
    }

    /// Register a callback and return its independent retirement handle.
    pub fn register_scalar_function_retained<F>(
        &self,
        name: &str,
        options: SQLFunctionOptions,
        function: F,
    ) -> Result<SQLFunctionRegistration, SQLFunctionRegistrationError>
    where
        F: SQLScalarFunction + 'static,
    {
        Self::validate_sql_function_options(options)?;
        let name = Self::normalize_sql_function_name(name)?;
        self.extensions
            .functions
            .register_scalar(name, Arc::new(function), options)
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
        self.register_table_function_retained(name, options, function)
            .map(drop)
            .map_err(Into::into)
    }

    /// Register a callback and return its independent retirement handle.
    pub fn register_table_function_retained<F>(
        &self,
        name: &str,
        options: SQLFunctionOptions,
        function: F,
    ) -> Result<SQLFunctionRegistration, SQLFunctionRegistrationError>
    where
        F: SQLTableFunction + 'static,
    {
        Self::validate_sql_function_options(options)?;
        let name = Self::normalize_sql_function_name(name)?;
        self.extensions
            .functions
            .register_table(name, retained::Table::wrap(function), options)
    }

    pub fn register_aggregate_function<F>(
        &self,
        name: &str,
        function: F,
    ) -> std::result::Result<(), SQLError>
    where
        F: SQLAggregateFactory + 'static,
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
        F: SQLAggregateFactory + 'static,
    {
        self.register_aggregate_function_retained(name, options, function)
            .map(drop)
            .map_err(Into::into)
    }

    /// Register a callback and return its independent retirement handle.
    pub fn register_aggregate_function_retained<F>(
        &self,
        name: &str,
        options: SQLFunctionOptions,
        function: F,
    ) -> Result<SQLFunctionRegistration, SQLFunctionRegistrationError>
    where
        F: SQLAggregateFactory + 'static,
    {
        Self::validate_sql_function_options(options)?;
        let name = Self::normalize_sql_function_name(name)?;
        self.extensions.functions.register_aggregate(
            name,
            retained::Aggregate::wrap(function),
            options,
        )
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
        Some(
            uqa_sql::expr::enums::render_host_arguments(Some(self), args).and_then(|args| {
                registration
                    .function
                    .call_with_context(self.query_runtime_view().function_context, &args)
            }),
        )
    }

    pub(crate) fn has_registered_scalar_functions(&self) -> bool {
        self.query_runtime_view().has_scalar_functions()
    }

    pub(crate) fn has_registered_scalar_function(&self, name: &str) -> bool {
        self.query_runtime_view().has_scalar_function(name)
    }

    pub(crate) fn has_registered_table_function(&self, name: &str) -> bool {
        self.query_runtime_view().has_table_function(name)
    }

    pub(crate) fn has_registered_aggregate_function(&self, name: &str) -> bool {
        self.query_runtime_view().has_aggregate_function(name)
    }

    pub(crate) fn registered_aggregate_function(
        &self,
        name: &str,
    ) -> Option<Arc<dyn SQLAggregateFactory>> {
        self.query_runtime_view()
            .lookup_aggregate_function(name)
            .map(|registration| {
                Arc::new(retained::BoundAggregate {
                    function: registration.function,
                    context: self.query_runtime_view().function_context.clone(),
                }) as Arc<dyn SQLAggregateFactory>
            })
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

#[cfg(test)]
mod tests;
