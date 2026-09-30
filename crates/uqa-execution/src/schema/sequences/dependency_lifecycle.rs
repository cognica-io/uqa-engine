//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inspect sequence dependents and publish ordered schema removals using native owners.
use crate::{
    catalog::{
        foreign::reads::ForeignTablesRead, sequence_introspection::SequenceIntrospectionCatalog,
    },
    schema::{
        constraints::ConstraintAlterContext,
        foreign_definitions::ForeignDefinitionContext,
        publication::{
            dependencies::{CatalogPublicationChanges, LoadedTableSchemas},
            removal::ColumnDropPublicationContext,
            SchemaPublicationContext,
        },
        view_dependencies::ViewDependencyContext,
    },
};
use std::{ops::Deref, sync::Arc};
use uqa_sql::{
    ast::ColumnDef, catalog::events::definition::lookup::EventLookupContext,
    schema::sequences::implicit_ownership::StoredSequenceNames,
};
use uqa_storage::{SequenceOwner, StorageBackendError, StorageBackendResult};
pub type SequenceColumnsRead<'a> = Box<dyn Deref<Target = Vec<ColumnDef>> + 'a>;
pub trait SequenceTableMetadata {
    fn object_id(&self) -> [u8; 16];
    fn columns(&self) -> SequenceColumnsRead<'_>;
}
pub trait SequenceDependencyCatalog {
    fn refresh_tables(&self) -> StorageBackendResult<()>;
    fn refresh_catalog(&self) -> StorageBackendResult<()>;
    fn table_entries(&self) -> Vec<(String, Arc<dyn SequenceTableMetadata>)>;
    fn resolve_table_name(&self, name: &str) -> StorageBackendResult<Option<String>>;
    fn table(&self, name: &str) -> StorageBackendResult<Option<Arc<dyn SequenceTableMetadata>>>;
    fn foreign_tables(&self) -> ForeignTablesRead<'_>;
}
pub struct SequenceDependencyContext<'a> {
    pub catalog: &'a dyn SequenceDependencyCatalog,
    pub sequences: &'a dyn SequenceIntrospectionCatalog,
    pub expressions: &'a dyn StoredSequenceNames,
    pub views: ViewDependencyContext<'a>,
    pub events: EventLookupContext<'a>,
    pub foreign: ForeignDefinitionContext<'a>,
    pub tables: &'a dyn LoadedTableSchemas,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub constraints: ConstraintAlterContext<'a>,
    pub schema: SchemaPublicationContext<'a>,
    pub columns: ColumnDropPublicationContext<'a>,
}
impl SequenceDependencyContext<'_> {
    /// Forget the sequence in the columns that record it as their source of values, after what uses it has been removed.
    pub fn detach_sequence_provenance(&self, sequence: &str) -> StorageBackendResult<()> {
        let mut catalog_changed = false;
        for (_, table) in self.tables.table_schemas() {
            let mut columns = table.columns();
            let table_changed =
                uqa_sql::schema::sequences::dependencies::detach_sequence_provenance(
                    &mut columns,
                    sequence,
                );
            if !table_changed {
                continue;
            }
            table.persist_columns(&columns)?;
            table.write_columns().publish(columns);
            catalog_changed = true;
        }
        if catalog_changed {
            self.changes.table_catalog_changed();
        }
        self.foreign
            .detach_foreign_table_sequence_provenance(sequence)?;
        Ok(())
    }
    pub fn sequence_owner_target(&self, owner: SequenceOwner) -> Option<(String, String, bool)> {
        self.catalog
            .table_entries()
            .into_iter()
            .find_map(|(table_name, table)| {
                if table.object_id() != owner.table_object_id {
                    return None;
                }
                table
                    .columns()
                    .iter()
                    .find(|column| column.object_id == Some(owner.column_object_id))
                    .map(|column| (table_name, column.name.clone(), false))
            })
            .or_else(|| {
                self.catalog
                    .foreign_tables()
                    .iter()
                    .find_map(|(relation, table)| {
                        if table.object_id != owner.table_object_id {
                            return None;
                        }
                        table
                            .columns
                            .iter()
                            .find(|column| column.object_id == Some(owner.column_object_id))
                            .map(|column| (relation.qualified_name(), column.name.clone(), true))
                    })
            })
    }
    pub fn resolve_stored_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        let mut refreshed = false;
        uqa_sql::schema::dependencies::rewrites::rewrite_sequence_function_references(
            expression,
            &mut |reference| {
                if !refreshed {
                    self.sequences
                        .refresh_sequences()
                        .map_err(|error| error.to_string())?;
                    refreshed = true;
                }
                *reference = self.expressions.stored_sequence_name(reference)?;
                Ok(())
            },
        )
        .map_err(StorageBackendError::Other)
    }
}
