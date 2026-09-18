//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain selected relation metadata and coherent sequence authority for privilege inquiry.

use crate::catalog::{
    context::CatalogContext,
    foreign::StoredForeignTable,
    projection::{
        foreign_table_relation_oid, resolve_regclass_kind_by_oid, sequence_relation_oid,
        snapshot_table_relation_oid, view_relation_oid,
    },
    sequence::snapshot::{SequenceReadSnapshot, SequenceSnapshotSource},
    view::StoredView,
};
use std::{cell::RefCell, collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_core::{RelationIdentity, Value};
use uqa_sql::{
    catalog::{
        resolution::RelationResolution,
        roles::{
            guards::RoleCatalogGuards, identity::RoleSubject, RoleDefinition, RoleReferenceNames,
        },
        security::{
            sequence_inquiry::{SequencePrivilegeInquiry, SequenceTablePrivilegeInquiry},
            table::TablePrivilegeCheck,
            table_inquiry::{
                ColumnPrivilegeRelation, ResolvedTablePrivilegeTarget, TablePrivilegeCatalog,
                TablePrivilegeInquiry,
            },
            BoundTableSecurity, TableSecurity,
        },
    },
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub type TableColumnsRead<'a> = Box<dyn Deref<Target = Vec<uqa_sql::ast::ColumnDef>> + 'a>;
pub trait TablePrivilegeState {
    fn role_owner(&self) -> uqa_sql::catalog::roles::RoleIdentity;
    fn columns(&self) -> TableColumnsRead<'_>;
    fn security(&self) -> BoundTableSecurity;
    fn column_names(&self) -> Vec<String>;
}
/// Borrowed lookups hold the registry guard; retained lookups keep the selected table generation after releasing it.
pub trait TablePrivilegeRead {
    fn security_entries(
        &self,
    ) -> Box<dyn Iterator<Item = (RelationIdentity, BoundTableSecurity)> + '_>;
    fn keys(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_>;
    fn get(&self, relation: &RelationIdentity) -> Option<&dyn TablePrivilegeState>;
    fn retained(&self, relation: &RelationIdentity) -> Option<Arc<dyn TablePrivilegeState>>;
}
pub type PrivilegeViewsRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, StoredView>> + 'a>;
pub type PrivilegeForeignTablesRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, StoredForeignTable>> + 'a>;
pub type PrivilegeForeignSecurityRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, BoundTableSecurity>> + 'a>;
pub trait TablePrivilegeRegistry:
    uqa_sql::catalog::security::system_relations::SystemRelationSecurityCatalog
{
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
    pub snapshots: &'a dyn SequenceSnapshotSource,
}
impl TablePrivilegeContext<'_> {
    pub fn has_table_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        let read = self.read();
        read.inquiry().has_table_privilege_value(arguments)
    }

    pub fn has_column_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        let read = self.read();
        read.inquiry().has_column_privilege_value(arguments)
    }

    fn read(&self) -> InquiryRead<'_, '_> {
        InquiryRead {
            context: self,
            sequence: RefCell::new(None),
        }
    }
}

struct InquiryRead<'a, 'catalog> {
    context: &'a TablePrivilegeContext<'catalog>,
    sequence: RefCell<Option<SequenceReadSnapshot>>,
}

impl InquiryRead<'_, '_> {
    fn inquiry(&self) -> TablePrivilegeInquiry<'_> {
        TablePrivilegeInquiry {
            names: self.context.names,
            roles: self.context.roles,
            sequences: self,
            catalog: self,
        }
    }

    fn sequence_snapshot(&self) -> Result<SequenceReadSnapshot, SQLError> {
        if let Some(snapshot) = self.sequence.borrow().as_ref() {
            return Ok(snapshot.clone());
        }
        let snapshot = self
            .context
            .snapshots
            .sequence_read_snapshot()
            .map_err(|error| {
                SQLError::Internal(format!(
                    "load sequence authority for relation privilege inquiry: {error}"
                ))
            })?;
        *self.sequence.borrow_mut() = Some(snapshot.clone());
        Ok(snapshot)
    }
}

impl SequenceTablePrivilegeInquiry for InquiryRead<'_, '_> {
    fn sequence_table_privileges(
        &self,
        relation: &RelationIdentity,
        subject: &dyn RoleSubject,
        checks: &[TablePrivilegeCheck],
    ) -> Result<bool, SQLError> {
        let snapshot = self.sequence_snapshot()?;
        if !snapshot.object_ids.contains_key(relation) {
            return Err(
                uqa_sql::catalog::security::sequence_inquiry::missing_sequence(
                    &relation.qualified_name(),
                ),
            );
        }
        snapshot
            .privileges(&self.context.sequences)
            .role_has_sequence_table_privileges(relation, subject, checks)
    }
}

impl TablePrivilegeCatalog for InquiryRead<'_, '_> {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        self.context
            .sequences
            .resolution
            .visible_relation_kind(reference)
    }
    fn table_privilege_security(
        &self,
        target: &ResolvedTablePrivilegeTarget,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<TableSecurity, SQLError> {
        match target {
            ResolvedTablePrivilegeTarget::System(relation) => self
                .context
                .registry
                .system_relation_security(*relation)
                .resolve(roles)
                .map_err(SQLError::Internal),
            ResolvedTablePrivilegeTarget::Table(relation) => self
                .context
                .registry
                .tables()
                .get(relation)
                .map(TablePrivilegeState::security)
                .ok_or_else(|| disappeared("table", relation))?
                .resolve(roles)
                .map_err(SQLError::Internal),
            ResolvedTablePrivilegeTarget::View(relation) => self
                .context
                .registry
                .views()
                .get(relation)
                .map(StoredView::security)
                .ok_or_else(|| disappeared("view", relation))?
                .resolve(roles)
                .map_err(SQLError::Internal),
            ResolvedTablePrivilegeTarget::ForeignTable(relation) => self
                .context
                .registry
                .foreign_security()
                .get(relation)
                .cloned()
                .ok_or_else(|| missing_foreign_table_security(relation))?
                .resolve(roles)
                .map_err(SQLError::Internal),
            ResolvedTablePrivilegeTarget::Sequence(_) => Err(SQLError::Internal(
                "sequence reached table-shaped privilege lookup".into(),
            )),
        }
    }

    fn column_privilege_relation(
        &self,
        target: &ResolvedTablePrivilegeTarget,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<ColumnPrivilegeRelation, SQLError> {
        match target {
            ResolvedTablePrivilegeTarget::System(relation) => Ok(ColumnPrivilegeRelation {
                relation: RelationIdentity::new(relation.namespace(), relation.name()),
                security: self
                    .context
                    .registry
                    .system_relation_security(*relation)
                    .resolve(roles)
                    .map_err(SQLError::Internal)?,
                columns: relation.column_names(),
                has_system_columns: relation.kind() == "table",
            }),
            ResolvedTablePrivilegeTarget::Table(relation) => {
                let table = self
                    .context
                    .registry
                    .tables()
                    .retained(relation)
                    .ok_or_else(|| disappeared("table", relation))?;
                let columns = table.column_names();
                Ok(ColumnPrivilegeRelation {
                    relation: relation.clone(),
                    security: table
                        .security()
                        .resolve(roles)
                        .map_err(SQLError::Internal)?,
                    columns,
                    has_system_columns: true,
                })
            }
            ResolvedTablePrivilegeTarget::View(relation) => {
                let view = self
                    .context
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
                    security: view.security.resolve(roles).map_err(SQLError::Internal)?,
                    columns,
                    has_system_columns: false,
                })
            }
            ResolvedTablePrivilegeTarget::ForeignTable(relation) => {
                let table = self
                    .context
                    .registry
                    .foreign_tables()
                    .get(relation)
                    .cloned()
                    .ok_or_else(|| disappeared("foreign table", relation))?;
                Ok(ColumnPrivilegeRelation {
                    relation: relation.clone(),
                    security: self.table_privilege_security(target, roles)?,
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
        if let Some(relation) =
            uqa_sql::catalog::SystemRelation::all().find(|relation| relation.oid() == oid)
        {
            return Ok(Some(ResolvedTablePrivilegeTarget::System(relation)));
        }
        self.context.registry.refresh_tables().map_err(|error| {
            SQLError::Internal(format!("load tables for privilege inquiry: {error}"))
        })?;
        self.context.registry.refresh_catalog().map_err(|error| {
            SQLError::Internal(format!("load views for privilege inquiry: {error}"))
        })?;
        let catalog = self.context.catalog.catalog_read_view();
        let resolution = self
            .context
            .catalog
            .session_execution_view()
            .relation_name_resolution();
        for relation in self.context.registry.tables().keys() {
            if snapshot_table_relation_oid(&catalog, &resolution, &relation.qualified_name())?
                == oid
            {
                return Ok(Some(ResolvedTablePrivilegeTarget::Table(relation.clone())));
            }
        }
        for (relation, view) in self.context.registry.views().iter() {
            if view_relation_oid(view) == oid {
                return Ok(Some(ResolvedTablePrivilegeTarget::View(relation.clone())));
            }
        }
        for (relation, table) in self.context.registry.foreign_tables().iter() {
            if foreign_table_relation_oid(table) == oid {
                return Ok(Some(ResolvedTablePrivilegeTarget::ForeignTable(
                    relation.clone(),
                )));
            }
        }
        let snapshot = self.sequence_snapshot()?;
        if let Some((relation, _)) = snapshot
            .object_ids
            .iter()
            .find(|(_, object_id)| sequence_relation_oid(**object_id) == oid)
        {
            return Ok(Some(ResolvedTablePrivilegeTarget::Sequence(
                relation.clone(),
            )));
        }
        if let Some((name, kind)) = resolve_regclass_kind_by_oid(&self.context.catalog, oid)? {
            if kind == "S" {
                return Ok(None);
            }
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

#[cfg(test)]
mod tests;
