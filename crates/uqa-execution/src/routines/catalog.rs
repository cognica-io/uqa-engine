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

use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{ast::CreateFunction, routines::SQLUserFunction};

pub(crate) const FUNCTIONS_METADATA_KEY: &str = "sql_functions_json";

pub fn persist_sql_functions_snapshot(
    catalog: Option<&dyn uqa_storage::CatalogFacade>,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
) -> Result<(), SQLError> {
    let Some(catalog) = catalog else {
        return Ok(());
    };
    let defs: BTreeMap<String, Vec<CreateFunction>> = registry
        .iter()
        .map(|(name, overloads)| {
            (
                name.clone(),
                overloads
                    .iter()
                    .map(|function| function.def.clone())
                    .collect(),
            )
        })
        .collect();
    let json = serde_json::to_string(&defs)
        .map_err(|err| SQLError::Internal(format!("serialize function catalog: {err}")))?;
    catalog
        .set_metadata(FUNCTIONS_METADATA_KEY, &json)
        .map_err(|err| SQLError::Internal(format!("persist function catalog: {err}")))
}
