//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata and publication adapters for SQL-owned index declarations and execution-owned builds.
use crate::{capabilities::RelationResolution, Engine};
use uqa_execution::schema::indexes::{
    creation::{IndexCreationContext, IndexCreationNamespace, IndexCreationPublication},
    IndexBuildContext,
};
use uqa_sql::{
    ast::{ColumnType, IndexKey},
    catalog::{errors::storage_error, index::IndexDefinition},
    schema::indexes::vectors::VectorIndexCatalog,
    SQLError,
};
use uqa_storage::vector_index::VectorIndexSpec;
impl Engine {
    pub(crate) fn index_creation_context(&self) -> IndexCreationContext<'_> {
        let runtime = self.query_runtime_view();
        IndexCreationContext {
            namespace: self,
            names: self,
            schema: self,
            bindings: self,
            unique: IndexBuildContext {
                catalog: self,
                reads: self,
                expressions: self.constraint_execution_context().index_expressions(),
                memory: runtime.settings,
            },
            vectors: self,
            publication: self,
            notices: runtime.notices,
        }
    }
}
impl IndexCreationNamespace for Engine {
    fn resolve_index_table_name(&self, name: &str) -> Result<Option<String>, SQLError> {
        self.try_resolve_index_table_name(name)
    }
    fn ensure_table_owner(&self, table: &str) -> Result<(), SQLError> {
        Engine::ensure_table_owner(self, table).map(|_| ())
    }
    fn ensure_creation_privilege(&self, table: &str) -> Result<(), SQLError> {
        self.ensure_existing_relation_creation_privilege(table)
    }
    fn relation_exists(&self, name: &str) -> Result<bool, SQLError> {
        Ok(matches!(
            self.resolve_bound_relation_kind(name)?,
            RelationResolution::Found(_, _)
        ))
    }
}
impl VectorIndexCatalog for Engine {
    fn resolve_table_name(&self, name: &str) -> Result<Option<String>, SQLError> {
        self.try_resolve_table_name(name)
            .map_err(|error| storage_error("CREATE INDEX", &error))
    }
    fn column_type(&self, table: &str, column: &str) -> Result<Option<ColumnType>, SQLError> {
        Engine::column_type(self, table, column)
            .map_err(|error| storage_error("CREATE INDEX", &error))
    }
    fn vector_index_names(&self, table: &str, column: &str) -> Result<Vec<String>, SQLError> {
        self.vector_catalog_index_names_for_column(table, column)
            .map_err(|error| storage_error("CREATE INDEX", &error))
    }
}
impl IndexCreationPublication for Engine {
    fn add_text_field(
        &self,
        table: &str,
        column: &str,
        analyzer: Option<&str>,
    ) -> Result<(), SQLError> {
        self.add_fts_field_with_analyzer(table, column.to_string(), analyzer)
            .map_err(|error| SQLError::Internal(format!("add_fts_field: {error}")))?;
        Ok(())
    }
    fn rebuild_vector_field(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
    ) -> Result<bool, SQLError> {
        self.rebuild_vector_field_with_spec(table, column, dimensions, spec)
            .map_err(|error| storage_error("CREATE INDEX vector field", &error))
    }
    fn register_index(
        &self,
        name: &str,
        method: &str,
        table: &str,
        keys: &[IndexKey],
        options: &[(String, String)],
        definition: &IndexDefinition,
    ) -> Result<(), SQLError> {
        self.register_catalog_index_definition(name, method, table, keys, options, definition)
            .map_err(|error| storage_error("CREATE INDEX", &error))?;
        Ok(())
    }
}
