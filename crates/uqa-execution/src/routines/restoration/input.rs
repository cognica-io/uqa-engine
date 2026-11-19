//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compile stored bodies in declaration order and publish each private overload group in its established signature order.

use super::{publish_restored, RoutineRegistry, RoutineRegistryState, RoutineRestoreContext};
use crate::routines::{
    catalog::input::binding_error,
    definition::{BindingStep, CatalogRoutineBinding, RoutineBodyCompilation},
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec, ProductionControl},
};
use uqa_sql::{
    plan::{AggregateClassifier, RetainedExecutablePlan},
    routines::{
        dependencies::RoutineDeclaration,
        lifecycle::restoration::{PendingRoutineDefinitions, RoutineRestorePlaceholders},
        SQLRoutine, StoredRoutineDefinition,
    },
    semantics::sets::placement_scope::SetPlacementScope,
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

pub struct RetainedRoutineFinalization {
    state: Budgeted<Box<Finalization>>,
}

struct Finalization {
    previous: RoutineRegistry,
    read: StorageReadControl,
    aggregates: Arc<dyn AggregateClassifier + Send + Sync>,
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
    Declaration(Bodies, Budgeted<Box<Declaration>>),
    Statement(
        Bodies,
        ImageDecoder<(Budgeted<Box<Declaration>>, RetainedExecutablePlan), StorageBackendError>,
    ),
    Group(ImageDecoder<Bodies, StorageBackendError>),
}

struct Declaration {
    definition: StoredRoutineDefinition,
    binding: Option<CatalogRoutineBinding>,
}

fn catalog_message(
    error: uqa_sql::SQLError,
    control: &ProductionControl<'_>,
) -> StorageBackendError {
    match error.into_retained_catalog_message(control) {
        Ok(uqa_sql::SQLError::RetainedRoutine(source)) => {
            StorageBackendError::CatalogBinding(source)
        }
        Ok(error) => binding_error(error),
        Err(error) => error.into(),
    }
}

impl RetainedRoutineFinalization {
    pub fn install(
        registry: &dyn RoutineRegistryState,
        prepared: RoutineRestorePlaceholders,
        read: &StorageReadControl,
        aggregates: Arc<dyn AggregateClassifier + Send + Sync>,
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
            aggregates,
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

    /// A call advances one declaration field, body statement, or private registry step.
    /// The compiler borrows the Engine only during that call. The owning private session retires
    /// both the placeholders and the saved predecessor if an open operation is abandoned.
    pub fn advance(
        mut self,
        context: &RoutineRestoreContext<'_>,
    ) -> StorageBackendResult<Option<Self>> {
        let Finalization {
            previous,
            read,
            aggregates,
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
            (Ok(()), Stage::Bind(bodies)) => bodies.advance(read),
            (Ok(()), Stage::Declaration(bodies, input)) => {
                bodies.advance_declaration(input, None, context, read, aggregates)
            }
            (Ok(()), Stage::Statement(bodies, mut input)) => {
                let lowered = {
                    let _placement = SetPlacementScope::stored_definition();
                    advance_one(&mut input)
                };
                match lowered {
                    Ok(Some((declaration, plan))) => bodies.advance_declaration(
                        declaration,
                        Some(plan),
                        context,
                        read,
                        aggregates,
                    ),
                    Ok(None) => Ok(Stage::Statement(bodies, input)),
                    Err(error) => Err(error),
                }
            }
            (Ok(()), Stage::Group(mut input)) => {
                advance_one(&mut input).map(|bodies| match bodies {
                    Some(bodies) => Stage::Bind(bodies),
                    None => Stage::Group(input),
                })
            }
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

fn advance_one<T: 'static>(
    input: &mut ImageDecoder<T, StorageBackendError>,
) -> StorageBackendResult<Option<T>> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

impl Bodies {
    fn advance(mut self, read: &StorageReadControl) -> StorageBackendResult<Stage> {
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
            let lease = read.memory().reserve(size_of::<Declaration>())?;
            let definition = StoredRoutineDefinition::new(routine.clone(), &control)
                .map_err(|error| catalog_message(error, &control))?;
            let input = Budgeted::new(
                Box::new(Declaration {
                    definition,
                    binding: None,
                }),
                lease,
            );
            return Ok(Stage::Declaration(self, input));
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

    fn advance_declaration(
        mut self,
        mut input: Budgeted<Box<Declaration>>,
        lowered: Option<RetainedExecutablePlan>,
        context: &RoutineRestoreContext<'_>,
        read: &StorageReadControl,
        aggregates: &Arc<dyn AggregateClassifier + Send + Sync>,
    ) -> StorageBackendResult<Stage> {
        let control =
            ProductionControl::new(read.memory(), read.cancellation(), read.cancellation());
        let declaration = input.as_mut();
        if let Some(binding) = &mut declaration.binding {
            match binding
                .advance_restored(
                    &context.definition,
                    &mut declaration.definition,
                    lowered,
                    &control,
                )
                .map_err(|error| catalog_message(error, &control))?
            {
                BindingStep::Pending => {}
                BindingStep::Statement(source) => {
                    let original = read.cancellation().clone();
                    let aggregates = Arc::clone(aggregates);
                    let lowering = ImageDecoder::new(
                        read.memory(),
                        read.cancellation(),
                        |mut reader| async move {
                            let mut work = reader.input();
                            let control =
                                ProductionControl::new(work.memory, &original, work.cancellation);
                            let plan = source
                                .lower(&input.definition, aggregates.as_ref(), &control, &mut work)
                                .await
                                .map_err(|error| catalog_message(error, &control))?;
                            Ok((input, plan))
                        },
                    )?;
                    return Ok(Stage::Statement(self, lowering));
                }
                BindingStep::Complete(bound) => {
                    let (input, _state_lease) = input.into_parts();
                    let routine = input
                        .definition
                        .publish(bound.body, &control)
                        .map_err(|error| catalog_message(error, &control))?;
                    self.compiled.push(routine)?;
                    self.migrated |= bound.changed;
                    return Ok(Stage::Bind(self));
                }
            }
        } else if declaration
            .definition
            .copy_next_field(&control)
            .map_err(|error| catalog_message(error, &control))?
        {
            declaration.binding = Some(
                CatalogRoutineBinding::new(
                    declaration.definition.declaration(),
                    RoutineBodyCompilation::Stored,
                    &control,
                )
                .map_err(|error| catalog_message(error, &control))?,
            );
        }
        Ok(Stage::Declaration(self, input))
    }
}
