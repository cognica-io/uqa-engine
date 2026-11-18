//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose native relationship predicates against one retained catalog view.

use super::{
    validate_authority, validate_constraint_bindings, validate_constraints,
    validate_event_bindings, validate_hierarchies, validate_index_bindings, validate_indexes,
    validate_name_uniqueness, validate_namespaces, validate_partition_bindings,
    validate_plan_relations, validate_routine_bindings, validate_routine_declarations,
    validate_routines, validate_sequence_functions, validate_sequences, validate_types,
};
use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    routines::{declaration::RoutineTypeCatalog, RoutineResolution},
    SQLError,
};

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    types: &dyn RoutineTypeCatalog,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let production = ProductionControl::new(memory, cancellation, cancellation);
    let snapshot = catalog.snapshot();
    validate_namespaces(catalog, &production)?;
    validate_name_uniqueness(snapshot, memory, cancellation)?;
    validate_authority(snapshot, memory, cancellation)?;
    crate::catalog::foreign::reference::validate_live_servers(
        snapshot.definitions.foreign_tables.values(),
        &snapshot.definitions.foreign_servers,
        &production,
    )?;
    validate_types(snapshot, memory, cancellation)?;
    validate_plan_relations(catalog, &production)?;
    validate_routine_declarations(snapshot, types, &production)?;
    validate_hierarchies(snapshot, memory, cancellation)?;
    validate_sequences(snapshot, memory, cancellation)?;
    validate_indexes(snapshot, memory, cancellation)?;
    validate_constraints(snapshot, memory, cancellation)?;
    validate_constraint_bindings(catalog, routines, memory, cancellation)?;
    validate_index_bindings(catalog, routines, memory, cancellation)?;
    validate_partition_bindings(catalog, routines, memory, cancellation)?;
    validate_routines(snapshot, memory, cancellation)?;
    validate_routine_bindings(catalog, routines, types, memory, cancellation)?;
    validate_event_bindings(catalog, routines, memory, cancellation)?;
    validate_sequence_functions(catalog, &production)
}
