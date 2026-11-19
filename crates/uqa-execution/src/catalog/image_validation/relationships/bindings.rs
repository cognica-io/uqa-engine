//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Binding validation borrows the same private lookup session across every work window.

use super::{Inputs, RelationshipValidation};
use crate::catalog::image_validation::{
    constraint_bindings, event_bindings, index_bindings, partition_bindings, routine_bindings,
    stored_expressions,
};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    catalog::CatalogValidation,
    routines::{declaration::RoutineTypeCatalog, RoutineResolution},
    SQLError,
};

#[derive(Clone, Copy)]
pub(super) enum Binding {
    Expressions,
    Constraints,
    Indexes,
    Partitions,
    Routines,
    Events,
}

impl Binding {
    pub(super) fn begin<S, R>(
        self,
        state: &RelationshipValidation,
        owner: S,
        services: fn(&S) -> &R,
    ) -> Result<CatalogValidation, SQLError>
    where
        S: Send + Sync + 'static,
        R: RoutineResolution + RoutineTypeCatalog + Send + Sync + 'static,
    {
        let memory = state.memory.clone();
        let control =
            ProductionControl::new(&state.memory, &state.cancellation, &state.cancellation);
        let catalog = state.catalog.clone();
        let original = state.cancellation.clone();
        macro_rules! reader {
            ($module:ident $(, $types:expr)?) => {
                CatalogValidation::new(&control, |mut work| async move {
                    $module::validate_with_work(&catalog, services(&owner), $($types,)? &memory, &original, &mut work).await
                })
            };
        }
        match self {
            Self::Expressions => reader!(stored_expressions),
            Self::Constraints => reader!(constraint_bindings),
            Self::Indexes => reader!(index_bindings),
            Self::Partitions => reader!(partition_bindings),
            Self::Routines => reader!(routine_bindings, || services(&owner)
                as &dyn RoutineTypeCatalog),
            Self::Events => reader!(event_bindings),
        }
    }

    pub(super) fn inline(self, input: &Inputs<'_>) -> Result<(), SQLError> {
        let Inputs {
            view,
            routines,
            types,
            memory,
            cancellation,
        } = *input;
        match self {
            Self::Expressions => stored_expressions::validate(view, routines, memory, cancellation),
            Self::Constraints => {
                constraint_bindings::validate(view, routines, memory, cancellation)
            }
            Self::Indexes => index_bindings::validate(view, routines, memory, cancellation),
            Self::Partitions => partition_bindings::validate(view, routines, memory, cancellation),
            Self::Routines => {
                routine_bindings::validate(view, routines, types, memory, cancellation)
            }
            Self::Events => event_bindings::validate(view, routines, memory, cancellation),
        }
    }
}
