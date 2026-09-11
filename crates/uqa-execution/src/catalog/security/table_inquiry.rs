//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live metadata and retained table generations for table and column privilege inquiry.

use crate::catalog::{
    context::CatalogContext,
    foreign::StoredForeignTable,
    projection::{
        foreign_table_relation_oid, resolve_regclass_kind_by_oid, snapshot_table_relation_oid,
        view_relation_oid,
    },
    view::StoredView,
};
use std::{collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::{
        resolution::RelationResolution,
        roles::{guards::RoleCatalogGuards, RoleReferenceNames},
        security::{
            sequence_inquiry::SequencePrivilegeInquiry,
            table_inquiry::{
                ColumnPrivilegeRelation, ResolvedTablePrivilegeTarget, TablePrivilegeCatalog,
                TablePrivilegeInquiry,
            },
            TableSecurity,
        },
    },
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub type TableColumnsRead<'a> = Box<dyn Deref<Target = Vec<uqa_sql::ast::ColumnDef>> + 'a>;
pub trait TablePrivilegeState {
    fn role_owner(&self) -> String;
    fn columns(&self) -> TableColumnsRead<'_>;
    fn security(&self) -> TableSecurity;
    fn column_names(&self) -> Vec<String>;
}
/// Borrowed lookups hold the registry guard; retained lookups keep the selected table generation after releasing it.
pub trait TablePrivilegeRead {
    fn security_entries(&self) -> Box<dyn Iterator<Item = (RelationIdentity, TableSecurity)> + '_>;
    fn keys(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_>;
    fn get(&self, relation: &RelationIdentity) -> Option<&dyn TablePrivilegeState>;
    fn retained(&self, relation: &RelationIdentity) -> Option<Arc<dyn TablePrivilegeState>>;
}
pub type PrivilegeViewsRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, StoredView>> + 'a>;
pub type PrivilegeForeignTablesRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, StoredForeignTable>> + 'a>;
pub type PrivilegeForeignSecurityRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, TableSecurity>> + 'a>;
pub trait TablePrivilegeRegistry {
    fn refresh_tables(&self) -> StorageBackendResult<()>;
    fn refresh_catalog(&self) -> StorageBackendResult<()>;
    fn tables(&self) -> Box<dyn TablePrivilegeRead + '_>;
    fn views(&self) -> PrivilegeViewsRead<'_>;
    fn foreign_tables(&self) -> PrivilegeForeignTablesRead<'_>;
    fn foreign_security(&self) -> PrivilegeForeignSecurityRead<'_>;
}
pub struct TablePrivilegeContext<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub sequences: SequencePrivilegeInquiry<'a>,
    pub catalog: CatalogContext<'a>,
    pub registry: &'a dyn TablePrivilegeRegistry,
}
impl TablePrivilegeContext<'_> {
    pub fn inquiry(&self) -> TablePrivilegeInquiry<'_> {
        TablePrivilegeInquiry {
            names: self.names,
            roles: self.roles,
            sequences: &self.sequences,
            catalog: self,
        }
    }
}
impl TablePrivilegeCatalog for TablePrivilegeContext<'_> {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        self.sequences.resolution.visible_relation_kind(reference)
    }
    fn table_privilege_security(
        &self,
        target: &ResolvedTablePrivilegeTarget,
    ) -> Result<TableSecurity, SQLError> {
        match target {
            ResolvedTablePrivilegeTarget::Table(relation) => self
                .registry
                .tables()
                .get(relation)
                .map(TablePrivilegeState::security)
                .ok_or_else(|| disappeared("table", relation)),
            ResolvedTablePrivilegeTarget::View(relation) => self
                .registry
                .views()
                .get(relation)
                .map(StoredView::security)
                .ok_or_else(|| disappeared("view", relation)),
            ResolvedTablePrivilegeTarget::ForeignTable(relation) => self
                .registry
                .foreign_security()
                .get(relation)
                .cloned()
                .ok_or_else(|| missing_foreign_table_security(relation)),
            ResolvedTablePrivilegeTarget::Sequence(_) => Err(SQLError::Internal(
                "sequence reached table-shaped privilege lookup".into(),
            )),
        }
    }

    fn column_privilege_relation(
        &self,
        target: &ResolvedTablePrivilegeTarget,
    ) -> Result<ColumnPrivilegeRelation, SQLError> {
        match target {
            ResolvedTablePrivilegeTarget::Table(relation) => {
                let table = self
                    .registry
                    .tables()
                    .retained(relation)
                    .ok_or_else(|| disappeared("table", relation))?;
                let columns = table.column_names();
                Ok(ColumnPrivilegeRelation {
                    relation: relation.clone(),
                    security: table.security(),
                    columns,
                    has_system_columns: true,
                })
            }
            ResolvedTablePrivilegeTarget::View(relation) => {
                let view = self
                    .registry
                    .views()
                    .get(relation)
                    .cloned()
                    .ok_or_else(|| disappeared("view", relation))?;
                let columns = view.output_columns.clone().ok_or_else(|| {
                    SQLError::Internal(format!(
                        "loaded view `{}` has no durable public column metadata",
                        relation.qualified_name()
                    ))
                })?;
                Ok(ColumnPrivilegeRelation {
                    relation: relation.clone(),
                    security: view.security(),
                    columns,
                    has_system_columns: false,
                })
            }
            ResolvedTablePrivilegeTarget::ForeignTable(relation) => {
                let table = self
                    .registry
                    .foreign_tables()
                    .get(relation)
                    .cloned()
                    .ok_or_else(|| disappeared("foreign table", relation))?;
                Ok(ColumnPrivilegeRelation {
                    relation: relation.clone(),
                    security: self.table_privilege_security(target)?,
                    columns: table
                        .columns
                        .iter()
                        .map(|column| column.name.clone())
                        .collect(),
                    has_system_columns: true,
                })
            }
            ResolvedTablePrivilegeTarget::Sequence(_) => Err(SQLError::Internal(
                "sequence reached table-shaped column lookup".into(),
            )),
        }
    }

    fn resolve_table_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<ResolvedTablePrivilegeTarget>, SQLError> {
        self.registry.refresh_tables().map_err(|error| {
            SQLError::Internal(format!("load tables for privilege inquiry: {error}"))
        })?;
        self.registry.refresh_catalog().map_err(|error| {
            SQLError::Internal(format!("load views for privilege inquiry: {error}"))
        })?;
        let catalog = self.catalog.catalog_read_view();
        let resolution = self
            .catalog
            .session_execution_view()
            .relation_name_resolution();
        for relation in self.registry.tables().keys() {
            if snapshot_table_relation_oid(&catalog, &resolution, &relation.qualified_name())?
                == oid
            {
                return Ok(Some(ResolvedTablePrivilegeTarget::Table(relation.clone())));
            }
        }
        for (relation, view) in self.registry.views().iter() {
            if view_relation_oid(view) == oid {
                return Ok(Some(ResolvedTablePrivilegeTarget::View(relation.clone())));
            }
        }
        for (relation, table) in self.registry.foreign_tables().iter() {
            if foreign_table_relation_oid(table) == oid {
                return Ok(Some(ResolvedTablePrivilegeTarget::ForeignTable(
                    relation.clone(),
                )));
            }
        }
        if let Some((_name, relation)) = self.sequences.resolution.sequence_privilege_oid(oid)? {
            return Ok(Some(ResolvedTablePrivilegeTarget::Sequence(relation)));
        }
        if let Some((name, kind)) = resolve_regclass_kind_by_oid(&self.catalog, oid)? {
            return Err(SQLError::Unsupported(format!(
                "has_table_privilege for {kind} `{name}` is not supported"
            )));
        }
        Ok(None)
    }
}

fn disappeared(kind: &str, relation: &RelationIdentity) -> SQLError {
    SQLError::Internal(format!(
        "{kind} `{}` disappeared",
        relation.qualified_name()
    ))
}

fn missing_foreign_table_security(relation: &RelationIdentity) -> SQLError {
    SQLError::Internal(format!(
        "foreign table `{}` has no loaded security metadata",
        relation.qualified_name()
    ))
}
