//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog metadata adapters for SQL declaration analysis and physical index validation.

use crate::Engine;
use uqa_sql::{
    ast::{ColumnDef, FunctionVolatility},
    SQLError,
};

impl uqa_sql::schema::SchemaExpressionCatalog for Engine {
    fn registered_runtime_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        Engine::registered_runtime_function_volatility(self, name)
    }
    fn schema_expression_columns(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, SQLError> {
        self.try_describe_table(table)
            .map_err(|error| SQLError::Internal(error.to_string()))
    }
}

impl uqa_sql::schema::inheritance::InheritanceCatalog for Engine {
    fn resolve_parent(&self, name: &str) -> Result<String, SQLError> {
        self.resolve_visible_table_reference(name)
    }
    fn declared_constraints(
        &self,
        table: &str,
    ) -> Result<uqa_sql::ast::TableConstraintSet, String> {
        self.try_declared_table_constraints(table)
            .map_err(|error| error.to_string())
    }
    fn check_definitions(&self, table: &str) -> Result<Vec<uqa_sql::ast::TableCheck>, String> {
        self.try_check_constraint_definitions(table)
            .map_err(|error| error.to_string())
    }
}

impl uqa_sql::schema::indexes::names::IndexNameCatalog for Engine {
    fn existing_constraint_keys(
        &self,
        table: &str,
    ) -> Result<Vec<uqa_sql::ast::TableKeyConstraint>, SQLError> {
        if self.try_resolve_bound_table_name(table)?.is_some() {
            self.try_key_constraints(table)
                .map_err(|error| SQLError::Internal(error.to_string()))
        } else {
            Ok(Vec::new())
        }
    }
    fn relation_name_available(&self, qualified_name: &str) -> Result<bool, SQLError> {
        Ok(matches!(
            self.resolve_bound_relation_kind(qualified_name)?,
            super::RelationResolution::MissingRelation
        ))
    }
}

impl uqa_execution::schema::indexes::IndexBuildCatalog for Engine {
    fn table_hierarchy(&self, table: &str) -> Result<uqa_sql::ast::TableHierarchy, SQLError> {
        self.try_table_hierarchy(table)
            .map_err(|error| uqa_sql::catalog::errors::storage_error("CREATE UNIQUE INDEX", &error))
    }
    fn scan_tables(&self, table: &str) -> Result<Vec<String>, SQLError> {
        self.hierarchy_scan_tables(table, true)
    }
}
