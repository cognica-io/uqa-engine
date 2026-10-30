//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind mutation row metadata and expression scopes to the selected engine generation.
use crate::session::StatementReadSnapshot;
use crate::Engine;
use uqa_core::DocId;
use uqa_execution::mutation::rows::context::{
    MutationExpressionContext, MutationRowCatalog, MutationRowContext, MutationTupleMetadata,
};
use uqa_sql::{ast::ColumnDef, RowSchema, SQLError};
use uqa_storage::DocumentMetadata;

impl MutationRowCatalog for Engine {
    fn column_definitions(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        self.describe_table_in_execution(table)
            .map_err(|error| error.to_string())
    }
    fn column_names(&self, table: &str) -> Result<Vec<String>, String> {
        self.table_columns_in_execution(table)
            .map_err(|error| error.to_string())
    }
    fn visit_column_names_with_control(
        &self,
        table: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(&str) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        self.visit_raw_table_column_names_with_control(table, control, read)
    }
    fn view_schema(&self, name: &str) -> Result<RowSchema, SQLError> {
        let definition = self
            .view_definition(name)?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        self.stored_view_schema(&definition)
    }
}
impl MutationTupleMetadata for Engine {
    fn table_relation_oid(&self, table: &str) -> Result<i64, SQLError> {
        if self.query_catalog_snapshot.is_some() {
            return uqa_execution::catalog::projection::table_relation_oid(
                &self.catalog_execution(),
                table,
            );
        }
        let relation = uqa_sql::catalog::resolution::bound_relation_identity(table)?;
        let ordinary = self
            .storage
            .tables
            .read()
            .get(&relation)
            .map(|state| state.relation_oids().relation);
        ordinary
            .or_else(|| {
                self.durable
                    .foreign_tables
                    .read()
                    .get(&relation)
                    .map(|state| state.relation_oids().relation)
            })
            .map(i64::from)
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))
    }
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
}
