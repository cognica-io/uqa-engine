//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the physical relation and its actual metadata/index guards during column deletion.
use crate::{Engine, TableState};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::{DocId, RelationIdentity};
use uqa_execution::schema::publication::removal::{
    ColumnDropCatalog, ColumnDropIndexes, ColumnDropPublicationContext, ColumnDropRows,
    ColumnDropRules, ColumnDropSequences, ColumnDropTable, IndexRowsRead, IndexRowsWrite,
    SchemaWrite, VectorIndexesWrite,
};
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint},
    catalog::events::PreparedRuleColumnDrop,
    schema::columns::removal_metadata::{
        ColumnDependencyEntries, ColumnDependencyState, ColumnsRead, ForeignKeysRead,
    },
    SQLError,
};
use uqa_storage::{document_store::Document, StorageBackendResult, ValueIndexKey};
impl Engine {
    pub(crate) fn column_drop_publication_context(&self) -> ColumnDropPublicationContext<'_> {
        ColumnDropPublicationContext {
            catalog: self,
            indexes: self,
            rows: self,
            rules: self,
            sequences: self,
            views: self,
            modes: self,
        }
    }
}
struct ColumnDropBinding<'a> {
    engine: &'a Engine,
    state: Arc<TableState>,
}
struct ColumnDependencyBinding(Arc<TableState>);
impl ColumnDependencyState for ColumnDependencyBinding {
    fn columns(&self) -> ColumnsRead<'_> {
        Box::new(self.0.columns.read())
    }
    fn foreign_keys(&self) -> ForeignKeysRead<'_> {
        Box::new(self.0.foreign_keys.read())
    }
}
impl ColumnDependencyState for ColumnDropBinding<'_> {
    fn columns(&self) -> ColumnsRead<'_> {
        Box::new(self.state.columns.read())
    }
    fn foreign_keys(&self) -> ForeignKeysRead<'_> {
        Box::new(self.state.foreign_keys.read())
    }
}
impl ColumnDropCatalog for Engine {
    fn resolve_table(&self, table: &str, action: &str) -> StorageBackendResult<Option<String>> {
        self.resolve_table_ddl_target(table, action)
    }
    fn table(&self, table: &str) -> StorageBackendResult<Option<Box<dyn ColumnDropTable + '_>>> {
        self.try_table(table).map(|state| {
            state.map(|state| {
                Box::new(ColumnDropBinding {
                    engine: self,
                    state,
                }) as Box<dyn ColumnDropTable>
            })
        })
    }
    fn entries(&self) -> ColumnDependencyEntries<'_> {
        self.table_entries()
            .into_iter()
            .map(|(name, state)| {
                (
                    name,
                    Box::new(ColumnDependencyBinding(state)) as Box<dyn ColumnDependencyState>,
                )
            })
            .collect()
    }
}
impl ColumnDropTable for ColumnDropBinding<'_> {
    fn object_id(&self) -> [u8; 16] {
        self.state.object_id()
    }
    fn clear_value_indexes(&self) {
        self.state.value_indexes.write().clear();
    }
    fn write_columns(&self) -> SchemaWrite<'_, ColumnDef> {
        Box::new(self.state.columns.write())
    }
    fn write_checks(&self) -> SchemaWrite<'_, TableCheck> {
        Box::new(self.state.table_checks.write())
    }
    fn write_keys(&self) -> SchemaWrite<'_, TableKeyConstraint> {
        Box::new(self.state.key_constraints.write())
    }
    fn write_foreign_keys(&self) -> SchemaWrite<'_, ForeignKey> {
        Box::new(self.state.foreign_keys.write())
    }
    fn remove_column_acl(&self, column: &str) {
        self.state.security.write().column_acls.remove(column);
    }
    fn remove_text_field(&self, column: &str) {
        self.state
            .fts_fields
            .write()
            .retain(|field| field != column);
    }
    fn write_vector_indexes(&self) -> VectorIndexesWrite<'_> {
        Box::new(self.state.vector_indexes.write())
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.state.document_store.read().doc_ids()
    }
    fn document(&self, id: DocId) -> StorageBackendResult<Option<Document>> {
        self.state.document_store.read().get(id)
    }
    fn persist_drop(&self, table: &str, column: &str) -> StorageBackendResult<()> {
        if !self.engine.is_persistent() {
            return Ok(());
        }
        if let Some(catalog) = self.engine.storage.catalog.as_ref() {
            catalog.drop_column_data(table, column)?;
        }
        self.engine.try_save_table_schema(table, &self.state)
    }
    fn mark_statistics_dirty(&self, table: &str) -> StorageBackendResult<()> {
        self.engine.mark_column_stats_dirty(table, &self.state)
    }
}
impl ColumnDropIndexes for Engine {
    fn read_indexes(&self) -> IndexRowsRead<'_> {
        Box::new(self.durable.catalog_indexes.read())
    }
    fn write_indexes(&self) -> IndexRowsWrite<'_> {
        Box::new(self.durable.catalog_indexes.write())
    }
    fn drop_catalog_index(&self, name: &RelationIdentity) -> StorageBackendResult<()> {
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.drop_catalog_index(name)?;
        }
        Ok(())
    }
    fn remove_value_index(&self, table: &str, name: &RelationIdentity) -> StorageBackendResult<()> {
        if let Some(state) = self.try_table(table)? {
            state
                .value_indexes
                .write()
                .remove(&ValueIndexKey::Index(name.qualified_name()));
        }
        Ok(())
    }
    fn remove_field_analyzer(&self, table: &str, column: &str) {
        self.durable
            .table_field_analyzers
            .write()
            .retain(|(name, field), _| !(name == table && field == column));
    }
    fn refresh_value_indexes(&self, table: &str) -> StorageBackendResult<()> {
        self.refresh_value_indexes_for_table(table)
    }
}
impl ColumnDropRows for Engine {
    fn rewrite(&self, table: &str, id: DocId, document: Document) -> Result<(), SQLError> {
        self.rewrite_document_for_schema_change(table, id, document)
    }
}
impl ColumnDropRules for Engine {
    fn prepare(&self, table: &str, column: &str) -> StorageBackendResult<PreparedRuleColumnDrop> {
        self.prepare_rule_column_drop(table, column)
    }
    fn finish(&self, prepared: PreparedRuleColumnDrop) -> StorageBackendResult<()> {
        self.finish_rule_column_drop(prepared)
    }
}
impl ColumnDropSequences for Engine {
    fn owned_by_column(
        &self,
        table: [u8; 16],
        column: [u8; 16],
    ) -> StorageBackendResult<BTreeSet<String>> {
        self.sequence_names_owned_by_column(table, column)
    }
    fn drop_owned(&self, sequence: &str, cascade: bool) -> StorageBackendResult<()> {
        self.drop_owned_sequence(sequence, cascade)
    }
}
