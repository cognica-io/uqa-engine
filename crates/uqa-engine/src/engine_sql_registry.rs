//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Arc, Engine, SQLAggregateFunction, SQLError, SQLScalarFunction, SQLTableFunction,
    SQLTableFunctionResult, Value,
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
        let name = Self::normalize_sql_function_name(name)?;
        self.sql_scalar_functions
            .write()
            .insert(name, Arc::new(function));
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
        let name = Self::normalize_sql_function_name(name)?;
        self.sql_table_functions
            .write()
            .insert(name, Arc::new(function));
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
        let name = Self::normalize_sql_function_name(name)?;
        self.sql_aggregate_functions
            .write()
            .insert(name, Arc::new(function));
        // Aggregate-vs-projection is a structural choice in `QueryPlan`.
        // Cached plans compiled before this registration must be rebound.
        self.clear_sql_statement_cache();
        self.rebind_prepared_plans();
        Ok(())
    }

    pub(crate) fn call_registered_scalar_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<Value, SQLError>> {
        let function = self
            .sql_scalar_functions
            .read()
            .get(&name.to_ascii_lowercase())
            .cloned()?;
        Some(function.call(args))
    }

    pub(crate) fn has_registered_scalar_functions(&self) -> bool {
        !self.sql_scalar_functions.read().is_empty()
    }

    pub(crate) fn has_registered_scalar_function(&self, name: &str) -> bool {
        self.sql_scalar_functions
            .read()
            .contains_key(&name.to_ascii_lowercase())
    }

    pub(crate) fn call_registered_table_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<SQLTableFunctionResult, SQLError>> {
        let function = self
            .sql_table_functions
            .read()
            .get(&name.to_ascii_lowercase())
            .cloned()?;
        Some(function.call(args))
    }

    pub(crate) fn has_registered_aggregate_function(&self, name: &str) -> bool {
        self.sql_aggregate_functions
            .read()
            .contains_key(&name.to_ascii_lowercase())
    }

    pub(crate) fn registered_aggregate_function(
        &self,
        name: &str,
    ) -> Option<Arc<dyn SQLAggregateFunction>> {
        self.sql_aggregate_functions
            .read()
            .get(&name.to_ascii_lowercase())
            .cloned()
    }
}
