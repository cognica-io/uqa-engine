//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind column rewrites to active row generations, field publication, and catalog state.
use crate::{session::StatementReadSnapshot, Engine};
use uqa_core::DocId;
use uqa_execution::schema::{
    columns::{
        backfill::{ColumnBackfillContext, ColumnBackfillState},
        generated::{GeneratedRewriteContext, GeneratedRewriteState},
        ColumnRewriteContext,
    },
    keys::KeyValidationContext,
};
use uqa_sql::{ast::ColumnType, SQLError};
use uqa_storage::StorageBackendResult;
impl Engine {
    pub(crate) fn column_rewrite_context(&self) -> ColumnRewriteContext<'_> {
        ColumnRewriteContext {
            columns: self,
            reads: self,
            types: self,
            expressions: self,
            writes: self,
        }
    }
    pub(crate) fn column_backfill_context(&self) -> ColumnBackfillContext<'_> {
        ColumnBackfillContext {
            rewrite: self.column_rewrite_context(),
            state: self,
            volatility: self,
        }
    }
    pub(crate) fn key_validation_context(&self) -> KeyValidationContext<'_> {
        KeyValidationContext {
            catalog: self,
            constraints: self.constraint_execution_context(),
        }
    }
    pub(crate) fn generated_rewrite_context(
        &self,
    ) -> GeneratedRewriteContext<'_, StatementReadSnapshot> {
        GeneratedRewriteContext {
            keys: self.key_validation_context(),
            assignment: self.mutation_assignment_context(),
            storage: self,
            state: self,
        }
    }
}
impl ColumnBackfillState for Engine {
    fn column_type(&self, table: &str, column: &str) -> StorageBackendResult<Option<ColumnType>> {
        Engine::column_type(self, table, column)
    }
    fn clear_missing_values(&self, table: &str) -> Result<(), SQLError> {
        for definition in self.require_table(table)?.columns.write().iter_mut() {
            definition.missing_value = None;
        }
        Ok(())
    }
}
impl GeneratedRewriteState for Engine {
    fn table_names(&self) -> StorageBackendResult<Vec<String>> {
        Engine::table_names(self)
    }
    fn advance_next_id(&self, table: &str, id: DocId) -> StorageBackendResult<()> {
        Engine::advance_next_id(self, table, id)
    }
}

impl Engine {
    pub(crate) fn column_addition_context(
        &self,
    ) -> uqa_execution::schema::columns::addition::ColumnAdditionContext<'_, StatementReadSnapshot>
    {
        uqa_execution::schema::columns::addition::ColumnAdditionContext {
            analysis: uqa_sql::schema::columns::addition::AddedColumnAnalysisContext {
                keys: self,
                schema: self,
                bindings: self,
                foreign_keys: self.foreign_key_definition_context(),
            },
            namespace: self.relation_creation_context(),
            state: self,
            transactions: self,
            generated: self.generated_rewrite_context(),
            backfill: self.column_backfill_context(),
        }
    }
}
impl uqa_sql::schema::columns::addition::AddedColumnKeys for Engine {
    fn try_key_constraints(
        &self,
        table: &str,
    ) -> Result<
        Vec<uqa_sql::ast::TableKeyConstraint>,
        uqa_sql::assignment::columns::ColumnCatalogError,
    > {
        Engine::try_key_constraints(self, table).map_err(|error| Box::new(error) as _)
    }
    fn try_foreign_keys(
        &self,
        table: &str,
    ) -> Result<Vec<uqa_sql::ast::ForeignKey>, uqa_sql::assignment::columns::ColumnCatalogError>
    {
        Engine::try_foreign_keys(self, table).map_err(|error| Box::new(error) as _)
    }
}

impl uqa_execution::schema::columns::addition::ColumnAdditionState for Engine {
    fn has_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.try_table_has_column(table, column)
    }
    fn create_vector_field(
        &self,
        table: &str,
        column: String,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        Engine::create_vector_field(self, table, column, dimensions)
    }
    fn add_text_field(&self, table: &str, column: String) -> Result<(), String> {
        self.add_fts_field(table, column)
    }
    fn set_missing_value(
        &self,
        table: &str,
        column: &str,
        value: Option<uqa_core::Value>,
    ) -> Result<(), SQLError> {
        let state = self.require_table(table)?;
        let mut columns = state.columns.write();
        let definition = columns
            .iter_mut()
            .find(|definition| definition.name == column)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "new column `{column}` disappeared during ALTER TABLE"
                ))
            })?;
        definition.missing_value = value;
        Ok(())
    }
    fn persist_schema(&self, table: &str) -> StorageBackendResult<bool> {
        self.try_persist_table_schema(table)
    }
}

impl Engine {
    pub(crate) fn column_alter_context(
        &self,
    ) -> uqa_execution::schema::columns::alteration::ColumnAlterContext<'_, StatementReadSnapshot>
    {
        uqa_execution::schema::columns::alteration::ColumnAlterContext {
            analysis: uqa_sql::schema::columns::alteration::ColumnAlterAnalysisContext {
                columns: self,
                keys: self,
                state: self,
                bindings: self.schema_dependency_binding_context(),
                constraint_types: self.constraint_type_context(),
            },
            fields: self,
            indexes: self,
            transactions: self,
            generated: self.generated_rewrite_context(),
            rewrite: self.column_rewrite_context(),
        }
    }
}
impl uqa_sql::schema::columns::alteration::ColumnChangeCatalog for Engine {
    fn has_column(
        &self,
        table: &str,
        column: &str,
    ) -> Result<bool, uqa_sql::assignment::columns::ColumnCatalogError> {
        self.try_table_has_column(table, column)
            .map_err(|error| Box::new(error) as _)
    }
    fn column_type(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Option<ColumnType>, uqa_sql::assignment::columns::ColumnCatalogError> {
        Engine::column_type(self, table, column).map_err(|error| Box::new(error) as _)
    }
    fn stored_columns(
        &self,
        table: &str,
    ) -> Result<Vec<uqa_sql::ast::ColumnDef>, uqa_sql::assignment::columns::ColumnCatalogError>
    {
        let state = self
            .table_entries()
            .into_iter()
            .find(|(name, _)| name == table)
            .map(|(_, state)| state)
            .ok_or_else(|| {
                uqa_storage::StorageBackendError::Other(format!("table `{table}` does not exist"))
            })?;
        let columns = state.columns.read().clone();
        Ok(columns)
    }
}
impl uqa_execution::schema::columns::alteration::ColumnIndexChanges for Engine {
    fn drop_vector_indexes(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.try_drop_vector_indexes_for_column(table, column)
    }
    fn rebuild_vector_index(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        self.try_rebuild_vector_index_for_column(table, column, dimensions)
    }
}
