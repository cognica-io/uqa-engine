//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live event metadata, retained catalog scopes, and declaration-analysis inputs.

use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, TableHierarchy},
    catalog::{
        events::definition::{EventAnalysisContext, EventForeignPrivileges, EventRelationCatalog},
        security::table::TableAclPrivilege,
        stored_view::StoredView,
        view::StoredViewKind,
    },
    RowSchema, SQLError,
};

impl Engine {
    pub(crate) fn event_analysis_context(&self) -> EventAnalysisContext<'_> {
        EventAnalysisContext {
            catalog: self,
            relations: self,
            sources: self,
            routines: self,
            authority: self,
            privileges: self,
            foreign_privileges: self,
            columns: self.stored_column_binding_context(),
            returning: self.returning_analysis_context(),
            stored_routines: self.catalog_routine_analysis_context(),
            namespaces: self,
            sequences: self,
            regroles: self,
        }
    }
}
impl EventRelationCatalog for Engine {
    fn event_relation_owner(
        &self,
        relation: &RelationIdentity,
    ) -> Result<(String, &'static str), SQLError> {
        if let Some(table) = self.storage.tables.read().get(relation) {
            return Ok((table.role_owner(), "table"));
        }
        if let Some(view) = self.durable.views.read().get(relation) {
            return Ok((
                view.role_owner.clone(),
                match view.kind {
                    StoredViewKind::View => "view",
                    StoredViewKind::Materialized => "materialized view",
                },
            ));
        }
        if self.durable.foreign_tables.read().contains_key(relation) {
            let owner = self
                .durable
                .foreign_table_security
                .read()
                .get(relation)
                .map(|security| security.role_owner.clone())
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "foreign trigger relation `{}` has no security metadata",
                        relation.qualified_name()
                    ))
                })?;
            return Ok((owner, "foreign table"));
        }
        Err(SQLError::Internal(format!(
            "event relation `{}` disappeared after resolution",
            relation.qualified_name()
        )))
    }
    fn view_kind(&self, relation: &RelationIdentity) -> Option<StoredViewKind> {
        self.durable
            .views
            .read()
            .get(relation)
            .map(|view| view.kind)
    }
    fn view(&self, relation: &RelationIdentity) -> Option<StoredView> {
        self.durable.views.read().get(relation).cloned()
    }
    fn foreign_columns(&self, relation: &RelationIdentity) -> Option<Vec<ColumnDef>> {
        self.durable
            .foreign_tables
            .read()
            .get(relation)
            .cloned()
            .map(|table| table.columns)
    }
    fn restored_catalog_view_definition(&self, name: &str) -> Result<Option<StoredView>, SQLError> {
        Engine::restored_catalog_view_definition(self, name)
    }
    fn stored_view_schema(&self, view: &StoredView) -> Result<RowSchema, SQLError> {
        Engine::stored_view_schema(self, view)
    }
    fn loaded_table_hierarchy(&self, relation: &RelationIdentity) -> Option<TableHierarchy> {
        Engine::loaded_table_hierarchy(self, relation)
    }
}
impl EventForeignPrivileges for Engine {
    fn ensure_foreign_table_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        Engine::ensure_foreign_table_privilege(self, name, privilege)
    }
}
