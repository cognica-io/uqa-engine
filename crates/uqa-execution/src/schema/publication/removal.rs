//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove physical column data through retained table generations and ordered catalog publication.
use crate::catalog::index::index_references_column;
use crate::schema::{columns::removal::ColumnRemovalViews, constraints::ConstraintModes};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
};
use uqa_core::{DocId, RelationIdentity};
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint},
    catalog::events::PreparedRuleColumnDrop,
    schema::columns::removal_metadata::{self, ColumnDependencyEntries, ColumnDependencyState},
};
use uqa_storage::{
    document_store::Document, CatalogIndexRow, StorageBackendError, StorageBackendResult,
    VectorIndex,
};
pub type SchemaWrite<'a, T> = Box<dyn DerefMut<Target = Vec<T>> + 'a>;
pub type VectorIndexesWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<String, Box<dyn VectorIndex>>> + 'a>;
pub type IndexRowsRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, CatalogIndexRow>> + 'a>;
pub type IndexRowsWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, CatalogIndexRow>> + 'a>;
/// One retained relation generation for the entire schema and physical-field removal.
pub trait ColumnDropTable: ColumnDependencyState {
    fn object_id(&self) -> [u8; 16];
    fn clear_value_indexes(&self);
    fn write_columns(&self) -> SchemaWrite<'_, ColumnDef>;
    fn write_checks(&self) -> SchemaWrite<'_, TableCheck>;
    fn write_keys(&self) -> SchemaWrite<'_, TableKeyConstraint>;
    fn write_foreign_keys(&self) -> SchemaWrite<'_, ForeignKey>;
    fn remove_column_acl(&self, column: &str);
    fn remove_text_field(&self, column: &str);
    fn write_vector_indexes(&self) -> VectorIndexesWrite<'_>;
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>>;
    fn document(&self, id: DocId) -> StorageBackendResult<Option<Document>>;
    fn persist_drop(&self, table: &str, column: &str) -> StorageBackendResult<()>;
    fn mark_statistics_dirty(&self, table: &str) -> StorageBackendResult<()>;
}
pub trait ColumnDropCatalog {
    fn resolve_table(&self, table: &str, action: &str) -> StorageBackendResult<Option<String>>;
    fn table(&self, table: &str) -> StorageBackendResult<Option<Box<dyn ColumnDropTable + '_>>>;
    fn entries(&self) -> ColumnDependencyEntries<'_>;
}
pub trait ColumnDropIndexes {
    fn read_indexes(&self) -> IndexRowsRead<'_>;
    fn write_indexes(&self) -> IndexRowsWrite<'_>;
    fn drop_catalog_index(&self, name: &RelationIdentity) -> StorageBackendResult<()>;
    fn remove_value_index(&self, table: &str, name: &RelationIdentity) -> StorageBackendResult<()>;
    fn remove_field_analyzer(&self, table: &str, column: &str);
    fn refresh_value_indexes(&self, table: &str) -> StorageBackendResult<()>;
}
pub trait ColumnDropRows {
    fn rewrite(&self, table: &str, id: DocId, document: Document) -> Result<(), uqa_sql::SQLError>;
}
pub trait ColumnDropRules {
    fn prepare(&self, table: &str, column: &str) -> StorageBackendResult<PreparedRuleColumnDrop>;
    fn finish(&self, prepared: PreparedRuleColumnDrop) -> StorageBackendResult<()>;
}
pub trait ColumnDropSequences {
    fn owned_by_column(
        &self,
        table: [u8; 16],
        column: [u8; 16],
    ) -> StorageBackendResult<BTreeSet<String>>;
    fn drop_owned(&self, sequence: &str, cascade: bool) -> StorageBackendResult<()>;
}
pub struct ColumnDropPublicationContext<'a> {
    pub catalog: &'a dyn ColumnDropCatalog,
    pub indexes: &'a dyn ColumnDropIndexes,
    pub rows: &'a dyn ColumnDropRows,
    pub rules: &'a dyn ColumnDropRules,
    pub sequences: &'a dyn ColumnDropSequences,
    pub views: &'a dyn ColumnRemovalViews,
    pub modes: &'a dyn ConstraintModes,
}
pub fn drop_column(
    context: &ColumnDropPublicationContext<'_>,
    table: &str,
    column: &str,
    cascade: bool,
) -> StorageBackendResult<bool> {
    let Some(table_name) = context
        .catalog
        .resolve_table(table, "ALTER TABLE DROP COLUMN")?
    else {
        return Ok(false);
    };
    let Some(state) = context.catalog.table(table)? else {
        return Ok(false);
    };
    if !state
        .columns()
        .iter()
        .any(|candidate| candidate.name == column)
    {
        return Ok(false);
    }
    let column_object_id = state
        .columns()
        .iter()
        .find(|candidate| candidate.name == column)
        .and_then(|candidate| candidate.object_id)
        .ok_or_else(|| {
            StorageBackendError::Other(format!(
                "column `{table_name}`.`{column}` has no object identity"
            ))
        })?;
    let owned_sequences = context
        .sequences
        .owned_by_column(state.object_id(), column_object_id)?;
    preflight_dependencies(context, &table_name, column)?;
    let prepared_rule_drop = context.rules.prepare(&table_name, column)?;
    state.clear_value_indexes();
    removal_metadata::remove_column_declarations(&mut state.write_columns(), column);
    removal_metadata::remove_column_checks(&mut state.write_checks(), column);
    removal_metadata::remove_column_keys(&mut state.write_keys(), column);
    removal_metadata::remove_column_foreign_keys(&mut state.write_foreign_keys(), column);
    state.remove_column_acl(column);
    state.remove_text_field(column);
    {
        let mut vectors = state.write_vector_indexes();
        if let Some(mut index) = vectors.remove(column) {
            index.clear()?;
        }
    }
    remove_catalog_indexes(context.indexes, &table_name, column)?;
    context.indexes.remove_field_analyzer(&table_name, column);
    for id in state.doc_ids()? {
        let Some(mut document) = state.document(id)? else {
            continue;
        };
        if document.remove(column).is_some() {
            context
                .rows
                .rewrite(&table_name, id, document)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
    }
    state.persist_drop(&table_name, column)?;
    context.rules.finish(prepared_rule_drop)?;
    for sequence in owned_sequences {
        context.sequences.drop_owned(&sequence, cascade)?;
    }
    state.mark_statistics_dirty(&table_name)?;
    context.indexes.refresh_value_indexes(&table_name)?;
    context
        .modes
        .prune()
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    Ok(true)
}
pub fn preflight_dependencies(
    context: &ColumnDropPublicationContext<'_>,
    table_name: &str,
    column: &str,
) -> StorageBackendResult<()> {
    let views = context.views.dependents(table_name, column)?;
    if !views.is_empty() {
        return Err(StorageBackendError::Other(format!(
            "ALTER TABLE DROP COLUMN `{table_name}`.`{column}` rejected: dependent view(s) {}",
            views.join(", ")
        )));
    }
    let target =
        RelationIdentity::from_legacy_name(table_name).map_err(StorageBackendError::Other)?;
    let entries = context.catalog.entries();
    removal_metadata::validate_column_dependencies(&target, table_name, column, &entries)
        .map_err(StorageBackendError::Other)?;
    // Inspect owned index metadata before the first mutation; retain the catalog read guard for the scan.
    for row in context.indexes.read_indexes().values() {
        if row.table_name == table_name {
            let _ = index_references_column(row, column)?;
        }
    }
    Ok(())
}
pub fn remove_catalog_indexes(
    indexes: &dyn ColumnDropIndexes,
    table: &str,
    column: &str,
) -> StorageBackendResult<()> {
    let mut rows = indexes.write_indexes();
    let mut removals = Vec::new();
    for (name, row) in rows.iter() {
        if row.table_name == table && index_references_column(row, column)? {
            removals.push(name.clone());
        }
    }
    for name in removals {
        indexes.drop_catalog_index(&name)?;
        indexes.remove_value_index(table, &name)?;
        rows.remove(&name);
    }
    Ok(())
}
