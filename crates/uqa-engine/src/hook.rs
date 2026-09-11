//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapter from the stable SQL expression hook to engine-owned capabilities.

use uqa_core::Value;
use uqa_sql::SQLError;

use super::Engine;

impl uqa_sql::expr::EngineHook for Engine {
    fn transaction_timestamp_micros(&self) -> Option<i64> {
        Some(Engine::transaction_timestamp_micros(self))
    }

    fn statement_timestamp_micros(&self) -> Option<i64> {
        Some(Engine::statement_timestamp_micros(self))
    }

    fn resolve_regtype_input(&self, name: &str) -> Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regtype_oid(&self.catalog_execution(), name)?
            .map(Some)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("type \"{name}\" does not exist"),
            })
    }
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &uqa_sql::ast::ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        uqa_sql::assignment::domain::cast_domain_value(self, value, source, target)
    }
    fn resolve_type_name(
        &self,
        name: &str,
    ) -> std::result::Result<Option<uqa_sql::ast::ColumnType>, String> {
        Ok(
            uqa_execution::catalog::projection::resolve_catalog_column_type(
                &self.catalog_execution(),
                name,
            ),
        )
    }

    fn resolve_regclass_input(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regclass_oid(&self.catalog_execution(), name)
    }

    fn resolve_regprocedure(&self, name: &str) -> std::result::Result<Option<i64>, String> {
        uqa_execution::catalog::projection::resolve_regprocedure_oid(
            &self.catalog_execution(),
            name,
        )
    }

    fn resolve_regrole(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regrole_oid(&self.catalog_execution(), name)
    }

    fn resolve_regnamespace(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regnamespace_oid(
            &self.catalog_execution(),
            name,
        )
    }

    fn resolve_regobject(
        &self,
        ty: &uqa_sql::ast::ColumnType,
        name: &str,
    ) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regobject_oid(
            &self.catalog_execution(),
            ty,
            name,
        )
    }

    fn resolve_regtype_output(
        &self,
        ty: &uqa_sql::ast::ColumnType,
        oid: i64,
    ) -> std::result::Result<Option<String>, String> {
        uqa_execution::catalog::projection::resolve_regtype_output(
            &self.catalog_execution(),
            ty,
            oid,
        )
    }

    fn nextval(&self, name: &str) -> std::result::Result<i64, SQLError> {
        self.nextval_sql(name)
    }

    fn currval(&self, name: &str) -> std::result::Result<i64, SQLError> {
        self.currval_sql(name)
    }

    fn lastval(&self) -> std::result::Result<i64, SQLError> {
        self.lastval_sql()
    }

    fn setval(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> std::result::Result<i64, SQLError> {
        self.setval_sql(name, value, is_called)
    }

    fn call_scalar_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<Value, SQLError>> {
        self.call_registered_scalar_function(name, args)
    }

    fn call_bound_builtin_function(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        uqa_execution::query::scalar_functions::call_bound_builtin(
            &self.scalar_function_context(),
            binding,
            args,
        )
    }

    fn has_scalar_functions(&self) -> bool {
        self.has_registered_scalar_functions()
    }

    fn current_schema(&self) -> std::result::Result<Option<String>, String> {
        self.current_schema_name()
            .map_err(|error| error.to_string())
    }

    fn current_user(&self) -> std::result::Result<Option<String>, String> {
        Ok(Some(self.current_user_name()))
    }

    fn session_user(&self) -> std::result::Result<Option<String>, String> {
        Ok(Some(self.session_user_name()))
    }

    fn current_schemas(
        &self,
        include_implicit: bool,
    ) -> std::result::Result<Option<Vec<String>>, String> {
        self.current_schema_names(include_implicit)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn random_value(&self) -> std::result::Result<Option<f64>, String> {
        Ok(Some(self.next_random_value()))
    }

    fn random_u64(&self) -> std::result::Result<Option<u64>, String> {
        Ok(Some(self.next_random_u64()))
    }

    fn set_random_seed(&self, seed: f64) -> std::result::Result<bool, String> {
        Engine::set_random_seed(self, seed)?;
        Ok(true)
    }

    fn call_user_function(
        &self,
        name: &str,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::capabilities::routine_invocation::call_user_scalar_function(self, name, args)
    }
}
