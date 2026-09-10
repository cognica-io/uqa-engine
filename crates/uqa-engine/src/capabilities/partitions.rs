//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata and expression adapters for SQL-owned partition semantics.

use crate::Engine;
use uqa_core::Value;
use uqa_sql::semantics::partition::{PartitionCatalog, PartitionContext, PartitionExpressions};
use uqa_sql::{
    ast::{ColumnDef, Expr, TableHierarchy},
    ResultRow, RowSchema, SQLError, SQLParam,
};

impl Engine {
    pub(crate) fn partition_context(&self) -> PartitionContext<'_> {
        PartitionContext {
            catalog: self,
            expressions: self,
            types: self,
        }
    }
}

impl PartitionCatalog for Engine {
    fn try_table_hierarchy(&self, table: &str) -> Result<TableHierarchy, String> {
        Engine::try_table_hierarchy(self, table).map_err(|error| error.to_string())
    }
    fn direct_hierarchy_children(&self, parent: &str) -> Result<Vec<String>, SQLError> {
        Engine::direct_hierarchy_children(self, parent)
    }
    fn try_resolve_table_name(&self, name: &str) -> Result<Option<String>, String> {
        Engine::try_resolve_table_name(self, name).map_err(|error| error.to_string())
    }
    fn try_describe_table(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Engine::try_describe_table(self, table).map_err(|error| error.to_string())
    }
}

impl PartitionExpressions for Engine {
    fn evaluate_bound(&self, expression: &Expr, params: &[SQLParam]) -> Result<Value, SQLError> {
        crate::sql::scalar::eval_lowered_expression(self, expression, None, params)
    }
    fn evaluate_row(
        &self,
        expression: &Expr,
        row: &ResultRow,
        schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        crate::sql::scalar::eval_lowered_expression_with_schema(
            self, expression, row, schema, params,
        )
    }
}
