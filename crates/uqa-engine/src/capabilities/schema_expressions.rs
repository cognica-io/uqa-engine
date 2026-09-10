//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata adapters for SQL-owned schema-expression validation.

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
