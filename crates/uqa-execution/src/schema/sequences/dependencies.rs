//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist sequence-reference rewrites across retained table, foreign-table, and view catalogs.
use crate::schema::publication::dependencies::{
    CatalogPublicationChanges, SchemaDependencyPublicationContext,
};
use std::{collections::BTreeMap, ops::Deref};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::stored_view::StoredView;
use uqa_sql::schema::sequences::dependencies::{
    rewrite_sequence_schema_references, rewritten_view_sequence_references,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub fn rewrite_sequence_schema_dependencies(
    context: &SchemaDependencyPublicationContext<'_>,
    from: &RelationIdentity,
    to: &str,
) -> StorageBackendResult<()> {
    let mut table_updates = Vec::new();
    for (_, state) in context.tables.table_schemas() {
        let mut columns = state.columns();
        let mut constraints = state.dependency_constraints();
        if rewrite_sequence_schema_references(&mut columns, &mut constraints.checks, from, to)
            .map_err(StorageBackendError::Other)?
        {
            table_updates.push((state, columns, constraints));
        }
    }
    let mut foreign_updates = Vec::new();
    for (relation, mut table) in context.foreign.foreign_tables() {
        if rewrite_sequence_schema_references(&mut table.columns, &mut table.checks, from, to)
            .map_err(StorageBackendError::Other)?
        {
            foreign_updates.push((relation, table));
        }
    }
    for (state, columns, constraints) in &mut table_updates {
        constraints.columns_declared = Some(state.columns_declared());
        state.persist_candidate(columns, constraints)?;
    }
    for (relation, table) in &foreign_updates {
        context.foreign.persist_foreign_table(relation, table)?;
    }
    for (state, columns, constraints) in &table_updates {
        state.publish_expressions(columns, &constraints.checks);
    }
    let foreign_tables_changed = !foreign_updates.is_empty();
    if foreign_tables_changed {
        context.foreign.publish_foreign_tables(foreign_updates);
    }
    if !table_updates.is_empty() {
        context.changes.table_catalog_changed();
    }
    if foreign_tables_changed {
        context.changes.catalog_registry_changed();
    }
    Ok(())
}

pub type ViewDefinitionsRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, StoredView>> + 'a>;
pub trait ViewCatalogPublication {
    fn synchronize_catalog(&self) -> StorageBackendResult<()>;
    fn view_definitions(&self) -> ViewDefinitionsRead<'_>;
    fn has_catalog(&self) -> bool;
    fn save_view_row(&self, row: &uqa_storage::ViewRow) -> StorageBackendResult<()>;
    fn publish_views(&self, updates: Vec<(RelationIdentity, StoredView)>);
}
pub struct ViewSequenceRewriteContext<'a> {
    pub views: &'a dyn ViewCatalogPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
}
pub fn rewrite_view_sequence_references(
    context: &ViewSequenceRewriteContext<'_>,
    from: &RelationIdentity,
    to: &str,
) -> StorageBackendResult<()> {
    context.views.synchronize_catalog()?;
    let mut rewritten_views = Vec::new();
    let views = context.views.view_definitions();
    for (relation, stored) in views.iter() {
        if let Some(rewritten) = rewritten_view_sequence_references(stored, from, to)
            .map_err(StorageBackendError::Other)?
        {
            rewritten_views.push((relation.clone(), rewritten));
        }
    }
    drop(views);
    if context.views.has_catalog() {
        for (relation, view) in &rewritten_views {
            if view.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                context
                    .views
                    .save_view_row(&crate::catalog::view::catalog_view_row(relation, view)?)?;
            }
        }
    }
    if !rewritten_views.is_empty() {
        context.views.publish_views(rewritten_views);
        context.changes.catalog_registry_changed();
    }
    Ok(())
}
