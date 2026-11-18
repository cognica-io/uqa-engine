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
use crate::catalog::{foreign::relationships as foreign, CatalogReadSnapshot, CatalogReadView};
use uqa_core::{
    image_io::ImageDecodeWindow,
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

const BEFORE_FOREIGN: &[Predicate] = &[
    |input| validate_namespaces(input.view, &input.production),
    |input| validate_name_uniqueness(input.snapshot, input.memory, input.cancellation),
    |input| validate_authority(input.snapshot, input.memory, input.cancellation),
];

const AFTER_FOREIGN: &[Predicate] = &[
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
    foreign: Option<ForeignCheck>,
}

enum ForeignCheck {
    Start,
    Reading(foreign::Input),
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
            remaining: BEFORE_FOREIGN,
            foreign: Some(ForeignCheck::Start),
        }
    }

    pub fn advance(
        self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
    ) -> Result<Option<Self>, SQLError> {
        self.advance_with_steps(routines, types, 1)
    }

    pub fn advance_with_steps(
        self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
        steps: usize,
    ) -> Result<Option<Self>, SQLError> {
        if steps == 0 {
            return Err(SQLError::Internal(
                "catalog validation requires a nonzero work window".into(),
            ));
        }
        self.next(routines, types, Some(steps))
    }

    fn next(
        mut self,
        routines: &dyn RoutineResolution,
        types: &dyn RoutineTypeCatalog,
        steps: Option<usize>,
    ) -> Result<Option<Self>, SQLError> {
        self.cancellation.check()?;
        let Some((predicate, remaining)) = self.remaining.split_first() else {
            return self.advance_foreign(steps);
        };
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
        Ok((!remaining.is_empty() || self.foreign.is_some()).then_some(self))
    }

    fn advance_foreign(mut self, steps: Option<usize>) -> Result<Option<Self>, SQLError> {
        let check = self
            .foreign
            .take()
            .expect("active foreign relationship check");
        if !foreign::has_bindings(self.catalog.snapshot()) {
            self.remaining = AFTER_FOREIGN;
            return Ok(Some(self));
        }
        if let Some(steps) = steps {
            let mut input = match check {
                ForeignCheck::Start => {
                    foreign::begin(&self.catalog, &self.memory, &self.cancellation)?
                }
                ForeignCheck::Reading(input) => input,
            };
            if input
                .as_mut()
                .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
                .is_none()
            {
                self.foreign = Some(ForeignCheck::Reading(input));
                return Ok(Some(self));
            }
        } else {
            foreign::inline(self.catalog.snapshot(), &self.memory, &self.cancellation)?;
        }
        self.remaining = AFTER_FOREIGN;
        Ok(Some(self))
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
        remaining = checks.next(routines, types, None)?;
    }
    Ok(())
}
