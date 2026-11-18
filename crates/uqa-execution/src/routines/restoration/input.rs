//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compile stored bodies in declaration order and publish each private overload group in its established signature order.

use super::{
    bind_stored, publish_restored, RoutineRegistry, RoutineRegistryState, RoutineRestoreContext,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec, ProductionControl},
};
use uqa_sql::{
    routines::{
        lifecycle::restoration::{PendingRoutineDefinitions, RoutineRestorePlaceholders},
        SQLRoutine,
    },
    SQLError,
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

pub struct RetainedRoutineFinalization {
    state: Budgeted<Box<Finalization>>,
}

struct Finalization {
    previous: RoutineRegistry,
    read: StorageReadControl,
    stage: Option<Stage>,
}

struct Bodies {
    definitions: PendingRoutineDefinitions,
    restored: RoutineRegistry,
    group: usize,
    compiled: BudgetedVec<SQLRoutine>,
    migrated: bool,
}

enum Stage {
    Bind(Bodies),
    Group(ImageDecoder<Bodies, StorageBackendError>),
}

impl RetainedRoutineFinalization {
    pub fn install(
        registry: &dyn RoutineRegistryState,
        prepared: RoutineRestorePlaceholders,
        read: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let lease = read.memory().reserve(size_of::<Finalization>())?;
        let (placeholders, definitions) = prepared.into_parts();
        let previous = std::mem::replace(&mut **registry.routines_write(), placeholders);
        let restored = previous
            .temporary_only()
            .into_mutation_budget(read.memory());
        let state = Finalization {
            previous,
            read: read.clone(),
            stage: Some(Stage::Bind(Bodies {
                definitions,
                restored,
                group: 0,
                compiled: BudgetedVec::new(read.memory()),
                migrated: false,
            })),
        };
        Ok(Self {
            state: Budgeted::new(Box::new(state), lease),
        })
    }

    /// A call compiles at most one declaration or advances the private registry by one work step.
    /// The compiler borrows the Engine only during that call. The owning private session retires
    /// both the placeholders and the saved predecessor if an open operation is abandoned.
    pub fn advance(
        mut self,
        context: &RoutineRestoreContext<'_>,
    ) -> StorageBackendResult<Option<Self>> {
        let Finalization {
            previous,
            read,
            stage,
        } = self.state.as_mut();
        let result = match (read.check(), stage.take().expect("active finalization")) {
            (Err(error), _) => Err(error),
            (Ok(()), Stage::Bind(bodies)) if bodies.definitions.group(bodies.group).is_none() => {
                let restored = bodies.restored.into_mutation_budget(previous.budget());
                return publish_restored(
                    context,
                    previous.clone(),
                    restored,
                    bodies.migrated,
                    false,
                )
                .map(|()| None);
            }
            (Ok(()), Stage::Bind(bodies)) => bodies.advance(context, read),
            (Ok(()), Stage::Group(mut input)) => input
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 },
                )
                .map(|bodies| match bodies {
                    Some(bodies) => Stage::Bind(bodies),
                    None => Stage::Group(input),
                }),
        };
        match result {
            Ok(next) => {
                *stage = Some(next);
                Ok(Some(self))
            }
            Err(error) => {
                **context.registry.routines_write() = previous.clone();
                Err(error)
            }
        }
    }
}

impl Bodies {
    fn advance(
        mut self,
        context: &RoutineRestoreContext<'_>,
        read: &StorageReadControl,
    ) -> StorageBackendResult<Stage> {
        let group = self
            .definitions
            .group(self.group)
            .expect("selected routine group");
        if let Some(routine) = group.get(self.compiled.len()) {
            if self.compiled.is_empty() {
                self.compiled.reserve(group.len())?;
            }
            let control =
                ProductionControl::new(read.memory(), read.cancellation(), read.cancellation());
            let (routine, changed) = routine
                .rebind_stored_with_control(&control, |def| bind_stored(context, def))
                .map_err(|error| match error {
                    SQLError::Cancelled(source) => source.into(),
                    error => StorageBackendError::Other(error.to_string()),
                })?;
            self.compiled.push(routine)?;
            self.migrated |= changed;
            return Ok(Stage::Bind(self));
        }
        let retained = read.clone();
        let input = ImageDecoder::new(
            read.memory(),
            read.cancellation(),
            |mut reader| async move {
                let mut input = reader.input();
                let compiled =
                    std::mem::replace(&mut self.compiled, BudgetedVec::new(retained.memory()));
                self.definitions
                    .publish_compiled_group_from_input(
                        self.group,
                        compiled,
                        &mut self.restored,
                        retained.cancellation(),
                        &mut input,
                    )
                    .await
                    .map_err(|error| {
                        crate::routines::catalog::input::restore_error(
                            error,
                            &ProductionControl::new(
                                retained.memory(),
                                retained.cancellation(),
                                input.cancellation,
                            ),
                        )
                    })?;
                self.group += 1;
                Ok(self)
            },
        )?;
        Ok(Stage::Group(input))
    }
}
