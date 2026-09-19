//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained catalog inputs for ordered role dependency checks.

use crate::{
    catalog::{
        security::{
            database::BoundDatabaseSecurity, BoundSchemaSecurity, BoundSequenceSecurity,
            BoundTableSecurity,
        },
        stored_view::StoredView,
    },
    routines::SQLUserFunction,
};
use std::{collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_core::RelationIdentity;

pub type RoleDependencyRead<'a, T> = Box<dyn Deref<Target = T> + 'a>;
/// Table security is read lazily at each visited role and relation pair.
pub trait RoleTableSecurity {
    fn security(&self) -> BoundTableSecurity;
    fn persistence(&self) -> crate::ast::RelationPersistence;
}
/// The iterator borrows its entries while the original table registry guard is retained.
pub trait RoleTablesRead {
    fn iter(&self) -> Box<dyn Iterator<Item = (&RelationIdentity, &dyn RoleTableSecurity)> + '_>;
}
pub trait RoleDependencyCatalog:
    crate::catalog::security::system_relations::SystemRelationSecurityCatalog
{
    fn database(&self) -> RoleDependencyRead<'_, BoundDatabaseSecurity>;
    fn schemas(&self) -> RoleDependencyRead<'_, BTreeMap<String, BoundSchemaSecurity>>;
    fn tables(&self) -> Box<dyn RoleTablesRead + '_>;
    fn views(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, StoredView>>;
    fn foreign_tables(
        &self,
    ) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, BoundTableSecurity>>;
    fn sequences(
        &self,
    ) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, BoundSequenceSecurity>>;
    fn routines(&self) -> RoleDependencyRead<'_, BTreeMap<String, Vec<Arc<SQLUserFunction>>>>;
}

pub trait TemporaryRoleDependencyCatalog: RoleDependencyCatalog {
    fn temporary_namespace_allocated(&self) -> bool;
    fn sequence_persistence(
        &self,
    ) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, crate::ast::RelationPersistence>>;
}
