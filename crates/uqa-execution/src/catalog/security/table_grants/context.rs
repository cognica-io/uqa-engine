//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained grant targets and state publication inputs for native privilege execution.
use super::super::{
    sequence_lifecycle::SequencePrivilegeContext,
    table_inquiry::{TablePrivilegeRegistry, TablePrivilegeState},
};
use crate::{
    catalog::view::ViewRegistryState,
    schema::{
        foreign_table_alteration::ForeignTableAlterPublication, namespaces::SchemaStatementWriter,
        publication::dependencies::CatalogPublicationChanges,
    },
};
use std::ops::DerefMut;
use uqa_core::RelationIdentity;
use uqa_sql::catalog::{
    roles::{guards::RoleCatalogGuards, RoleReferenceNames},
    security::{
        grants::GrantNamespace, table_grants::targets::TableGrantResolution, TableSecurity,
    },
};
use uqa_storage::{CatalogFacade, StorageBackendResult};
pub type TableSecurityWrite<'a> = Box<dyn DerefMut<Target = TableSecurity> + 'a>;
pub trait TableGrantState: TablePrivilegeState {
    fn security_write(&self) -> TableSecurityWrite<'_>;
    fn persist_security(&self, name: &str, security: &TableSecurity) -> StorageBackendResult<()>;
}
pub trait TableGrantRead<'a> {
    fn keys(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_>;
    fn retained(&self, relation: &RelationIdentity) -> Option<Box<dyn TableGrantState + 'a>>;
}
pub trait TableGrantRegistry {
    fn tables(&self) -> Box<dyn TableGrantRead<'_> + '_>;
}
pub use crate::catalog::notices::CatalogNotices as TableGrantNotices;
pub struct TableGrantContext<'a> {
    pub writer: &'a dyn SchemaStatementWriter,
    pub resolution: &'a dyn TableGrantResolution,
    pub namespaces: &'a dyn GrantNamespace,
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub registry: &'a dyn TablePrivilegeRegistry,
    pub tables: &'a dyn TableGrantRegistry,
    pub views: &'a dyn ViewRegistryState,
    pub foreign: &'a dyn ForeignTableAlterPublication,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub notices: &'a dyn TableGrantNotices,
    pub sequences: SequencePrivilegeContext<'a>,
}
pub trait TableGrantInputs {
    fn table_grant_context(&self) -> TableGrantContext<'_>;
}
