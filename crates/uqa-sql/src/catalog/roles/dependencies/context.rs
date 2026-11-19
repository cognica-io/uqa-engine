//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained catalog inputs for ordered role dependency checks.

use crate::catalog::security::schema_registry::SchemaRegistry;
use crate::catalog::security::{
    database::BoundDatabaseSecurity, sequence_registry::SequenceSecurities, BoundTableSecurity,
};
use std::ops::Deref;
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
    fn schemas(&self) -> RoleDependencyRead<'_, SchemaRegistry>;
    fn tables(&self) -> Box<dyn RoleTablesRead + '_>;
    fn views(&self) -> RoleDependencyRead<'_, crate::catalog::stored_view::ViewDefinitions>;
    fn foreign_tables(
        &self,
    ) -> RoleDependencyRead<'_, crate::catalog::security::foreign_tables::ForeignTableSecurities>;
    fn sequences(&self) -> RoleDependencyRead<'_, SequenceSecurities>;
    fn routines(&self) -> RoleDependencyRead<'_, crate::routines::registry::RoutineRegistry>;
    fn domains(
        &self,
    ) -> RoleDependencyRead<
        '_,
        crate::catalog::type_definitions::TypeDefinitions<crate::catalog::domain::StoredDomain>,
    >;
}

pub trait TemporaryRoleDependencyCatalog: RoleDependencyCatalog {
    fn temporary_namespace_allocated(&self) -> bool;
}
