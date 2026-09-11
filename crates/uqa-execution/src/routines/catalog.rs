//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine registry snapshots, retained write guards, and durable publication.

use std::ops::DerefMut;
use uqa_sql::{routines::lifecycle::RoutineRegistry, SQLError};

pub type RoutineRegistryWrite<'a> = Box<dyn DerefMut<Target = RoutineRegistry> + 'a>;
pub trait RoutineRegistryState {
    fn routine_snapshot(&self) -> RoutineRegistry;
    fn routines_write(&self) -> RoutineRegistryWrite<'_>;
}
pub trait RoutineRegistryPublication {
    fn persist_routine_definitions(&self, registry: &RoutineRegistry) -> Result<(), SQLError>;
}

#[derive(Clone, Copy)]
pub struct RoutineMutationContext<'a> {
    pub writer: &'a dyn crate::schema::namespaces::SchemaStatementWriter,
    pub names: &'a dyn uqa_sql::routines::lifecycle::names::RoutineNameCatalog,
    pub roles: &'a dyn crate::catalog::security::roles::RoleCatalogGuards,
    pub registry: &'a dyn RoutineRegistryState,
    pub publication: &'a dyn RoutineRegistryPublication,
    pub changes: &'a dyn crate::schema::namespaces::NamespaceCatalogChanges,
}
