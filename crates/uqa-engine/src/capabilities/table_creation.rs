//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind table creation to active query generations and schema publication state.
use crate::{session::StatementReadSnapshot, Engine};
use uqa_core::DocId;
use uqa_execution::mutation::publication::DocumentVectors;
use uqa_execution::query::CteScope;
use uqa_execution::schema::ctas::{
    CreateTableAsContext, TableAsNamespace, TableAsPublication, TableAsQuerySource,
};
use uqa_sql::catalog::errors::storage_error;
use uqa_sql::{
    ast::{ColumnDef, OnCommitAction, RelationPersistence},
    plan::QueryPlan,
    SQLError, SQLParam, SQLResult,
};
use uqa_storage::document_store::Document;
impl Engine {
    pub(crate) fn create_table_as_context<'a>(
        &'a self,
        analysis_scope: &'a CteScope<StatementReadSnapshot>,
    ) -> CreateTableAsContext<'a, StatementReadSnapshot> {
        CreateTableAsContext {
            analysis_scope,
            routines: self,
            queries: self,
            namespace: self,
            publication: self,
            vectors: self,
        }
    }
}
impl TableAsQuerySource for Engine {
    fn optimize(&self, plan: &QueryPlan) -> Result<QueryPlan, SQLError> {
        crate::sql::optimize_engine_query(self, plan)
    }
    fn execute(&self, plan: &QueryPlan, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
        let mut scope = super::query_scope::new_for_current_routine(self);
        uqa_execution::query::statement::execute_query_plan_with_ctes(
            &self.query_execution_context(),
            plan,
            params,
            &mut scope,
        )
    }
}
impl TableAsNamespace for Engine {
    fn ensure_temporary_privilege(&self) -> Result<(), SQLError> {
        self.ensure_temporary_relation_creation_privilege()
    }
    fn temporary_target_name(&self, name: &str) -> Result<String, SQLError> {
        self.try_temporary_relation_name_for_create(name)
    }
    fn target_name(&self, name: &str) -> Result<String, SQLError> {
        self.resolve_relation_name_for_sql_create(name)
    }
    fn relation_exists(&self, name: &str) -> Result<bool, SQLError> {
        self.relation_kind_at(name)
            .map(|kind| kind.is_some())
            .map_err(|error| storage_error("CREATE TABLE AS", &error))
    }
    fn ensure_create_privilege(&self, name: &str) -> Result<(), SQLError> {
        self.ensure_relation_creation_privilege(name)
    }
    fn prepare_writer(&self) -> Result<bool, SQLError> {
        self.prepare_explicit_transaction_writer()
    }
}
impl TableAsPublication for Engine {
    fn create_relation(
        &self,
        name: &str,
        persistence: RelationPersistence,
        on_commit: OnCommitAction,
    ) -> Result<(), SQLError> {
        self.create_table_with_lifecycle(
            name,
            uqa_analysis::analyzer::standard_analyzer("english"),
            Vec::new(),
            persistence,
            on_commit,
        )
        .map_err(|error| storage_error("CREATE TABLE AS", &error))
    }
    fn create_vector_field(
        &self,
        name: &str,
        column: &str,
        dimensions: u32,
    ) -> Result<bool, SQLError> {
        Engine::create_vector_field(self, name, column, dimensions)
            .map_err(|error| storage_error("CREATE TABLE AS vector field", &error))
    }
    fn publish_columns(&self, name: &str, columns: &[ColumnDef]) -> Result<(), SQLError> {
        let table = self
            .try_table(name)
            .map_err(|error| storage_error("CREATE TABLE AS schema", &error))?
            .ok_or_else(|| {
                SQLError::Internal(format!("new CREATE TABLE AS relation `{name}` disappeared"))
            })?;
        *table.columns.write() = columns.to_vec();
        self.try_persist_table_schema(name)
            .map_err(|error| storage_error("CREATE TABLE AS schema", &error))?;
        Ok(())
    }
    fn insert_document(
        &self,
        table: &str,
        id: DocId,
        document: Document,
        vectors: DocumentVectors,
    ) -> Result<(), SQLError> {
        self.add_document_with_vector_values(table, id, document, vectors)
    }
}

impl Engine {
    pub(crate) fn table_declaration_context(
        &self,
    ) -> uqa_sql::schema::table_creation::declaration::CreateTableAnalysisContext<'_> {
        uqa_sql::schema::table_creation::declaration::CreateTableAnalysisContext {
            types: self,
            schema: self,
            bindings: self,
            inheritance: uqa_sql::schema::inheritance::InheritanceContext {
                catalog: self,
                partitions: self.partition_context(),
                roles: self,
            },
            index_names: self,
            foreign_keys: self.foreign_key_definition_context(),
        }
    }
}
