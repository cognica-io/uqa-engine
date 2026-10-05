//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Separate catalog, authority, and publication services for routine removal.

use crate::{
    catalog::{context::CatalogContext, security::roles::RoleCatalogGuards},
    schema::namespaces::NamespaceCatalogChanges,
};
use uqa_sql::routines::lifecycle::names::RoutineNameCatalog;

pub use crate::routines::catalog::{
    RoutineRegistryPublication, RoutineRegistryState, RoutineRegistryWrite,
};
pub trait RoutineDropNotices {
    fn routine_drop_notice(&self, notice: uqa_sql::SQLNotice);
}
pub struct RoutineRemovalContext<'a> {
    pub deletion: &'a dyn crate::schema::deletion::CatalogRemovalInputs,
    pub names: &'a dyn RoutineNameCatalog,
    pub registry: &'a dyn RoutineRegistryState,
    pub publication: &'a dyn RoutineRegistryPublication,
    pub roles: &'a dyn RoleCatalogGuards,
    pub catalog: CatalogContext<'a>,
    pub bodies: crate::routines::rewrites::RoutineRewriteContext<'a>,
    pub notices: &'a dyn RoutineDropNotices,
    pub changes: &'a dyn NamespaceCatalogChanges,
}
