//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind mutation row metadata and expression scopes to the selected engine generation.
use crate::session::StatementReadSnapshot;
use crate::Engine;
use uqa_core::DocId;
use uqa_execution::mutation::{
    rows::context::{
        MutationExpressionContext, MutationRowCatalog, MutationRowContext, MutationTupleMetadata,
    },
    views::ViewRowContext,
};
use uqa_sql::{ast::ColumnDef, RowSchema, SQLError};
use uqa_storage::DocumentMetadata;

impl MutationRowCatalog for Engine {
    fn column_definitions(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        self.try_describe_table(table)
            .map_err(|error| error.to_string())
    }
    fn column_names(&self, table: &str) -> Result<Vec<String>, String> {
        self.try_table_columns(table)
            .map_err(|error| error.to_string())
    }
    fn view_schema(&self, name: &str) -> Result<RowSchema, SQLError> {
        let definition = self
            .view_definition(name)?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        self.stored_view_schema(&definition)
    }
}
impl MutationTupleMetadata for Engine {
    fn document_metadata(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<DocumentMetadata>, SQLError> {
        self.get_query_document_metadata(table, doc_id)
    }
    fn tuple_version_xid(&self) -> Result<u32, SQLError> {
        self.tuple_version_xid()
    }
}
impl Engine {
    pub(crate) fn mutation_row_context(&self) -> MutationRowContext<'_> {
        MutationRowContext {
            catalog: self.catalog_execution(),
            relations: self,
            tuples: self,
        }
    }
    pub(crate) fn mutation_expression_context(
        &self,
    ) -> MutationExpressionContext<'_, StatementReadSnapshot> {
        MutationExpressionContext {
            types: self,
            expressions: self,
        }
    }
    pub(crate) fn view_row_context(&self) -> ViewRowContext<'_, StatementReadSnapshot> {
        ViewRowContext {
            rewrite: self.view_rewrite_context(),
            rows: self.mutation_row_context(),
            expressions: self.mutation_expression_context(),
        }
    }
}
