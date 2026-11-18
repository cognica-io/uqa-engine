//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered native relationship checks against one immutable catalog view.

use super::{
    validate_authority, validate_constraint_bindings, validate_constraints,
    validate_event_bindings, validate_hierarchies, validate_index_bindings, validate_indexes,
    validate_name_uniqueness, validate_namespaces, validate_partition_bindings,
    validate_plan_relations, validate_routine_bindings, validate_routine_declarations,
    validate_routines, validate_sequence_functions, validate_sequences, validate_types,
};
use crate::catalog::{CatalogReadSnapshot, CatalogReadView};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    routines::{declaration::RoutineTypeCatalog, RoutineResolution},
    SQLError,
};

struct Inputs<'a> {
    view: &'a CatalogReadView,
    snapshot: &'a CatalogReadSnapshot,
    routines: &'a dyn RoutineResolution,
    types: &'a dyn RoutineTypeCatalog,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    production: ProductionControl<'a>,
}

type Predicate = fn(&Inputs<'_>) -> Result<(), SQLError>;

const PREDICATES: &[Predicate] = &[
    |input| validate_namespaces(input.view, &input.production),
    |input| validate_name_uniqueness(input.snapshot, input.memory, input.cancellation),
    |input| validate_authority(input.snapshot, input.memory, input.cancellation),
    |input| {
        crate::catalog::foreign::reference::validate_live_wrappers(
            &input.snapshot.definitions.foreign_servers,
            &input.snapshot.definitions.foreign_wrappers,
            &input.production,
        )
    },
    |input| {
        crate::catalog::foreign::wrappers::validate_callback_bindings(
            input.snapshot,
            &input.production,
        )
    },
    |input| {
        crate::catalog::foreign::reference::validate_live_servers(
            input.snapshot.definitions.foreign_tables.values(),
            &input.snapshot.definitions.foreign_servers,
            &input.production,
        )
    },
    |input| validate_types(input.snapshot, input.memory, input.cancellation),
    |input| validate_plan_relations(input.view, &input.production),
    |input| {
        super::validate_stored_expressions(
            input.view,
            input.routines,
            input.memory,
            input.cancellation,
        )
    },
    |input| validate_routine_declarations(input.snapshot, input.types, &input.production),
    |input| validate_hierarchies(input.snapshot, input.memory, input.cancellation),
    |input| validate_sequences(input.snapshot, input.memory, input.cancellation),
    |input| validate_indexes(input.snapshot, input.memory, input.cancellation),
    |input| validate_constraints(input.snapshot, input.memory, input.cancellation),
    |input| {
        validate_constraint_bindings(input.view, input.routines, input.memory, input.cancellation)
    },
    |input| validate_index_bindings(input.view, input.routines, input.memory, input.cancellation),
    |input| {
        validate_partition_bindings(input.view, input.routines, input.memory, input.cancellation)
    },
    |input| validate_routines(input.snapshot, input.memory, input.cancellation),
    |input| {
        validate_routine_bindings(
            input.view,
            input.routines,
            input.types,
            input.memory,
            input.cancellation,
        )
    },
    |input| validate_event_bindings(input.view, input.routines, input.memory, input.cancellation),
    |input| validate_sequence_functions(input.view, &input.production),
];

/// The outstanding predicates of an unpublished catalog. Each advance borrows the private
/// session only for its own call and preserves the same first error as synchronous validation.
pub struct RelationshipValidation {
    catalog: CatalogReadView,
    memory: MemoryBudget,
    cancellation: CancellationToken,
    remaining: &'static [Predicate],
}

impl RelationshipValidation {
    pub fn new(
        catalog: CatalogReadView,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Self {
        Self {
            catalog,
            memory: memory.clone(),
            cancellation: cancellation.clone(),
            remaining: PREDICATES,
        }
    }

    pub fn advance(
        mut self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
    ) -> Result<Option<Self>, SQLError> {
        self.cancellation.check()?;
        let (predicate, remaining) = self
            .remaining
            .split_first()
            .expect("active catalog validation");
        predicate(&Inputs {
            view: &self.catalog,
            snapshot: self.catalog.snapshot(),
            routines,
            types,
            memory: &self.memory,
            cancellation: &self.cancellation,
            production: ProductionControl::new(
                &self.memory,
                &self.cancellation,
                &self.cancellation,
            ),
        })?;
        self.cancellation.check()?;
        self.remaining = remaining;
        Ok((!remaining.is_empty()).then_some(self))
    }
}

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    types: &dyn RoutineTypeCatalog,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let mut remaining = Some(RelationshipValidation::new(
        catalog.clone(),
        memory,
        cancellation,
    ));
    while let Some(checks) = remaining {
        remaining = checks.advance(routines, types)?;
    }
    Ok(())
}
