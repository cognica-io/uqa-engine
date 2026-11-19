//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Placeholder publication shares original declarations while retaining the original finalization order.

use super::{
    checkpoint, CanonicalRoutineDefinitions, CanonicalRoutineGroup, Comparison,
    RoutineRestoreInputError,
};
use crate::routines::{
    registry::RoutineRegistry, routine_signature_types_from_input, CompiledFunctionBody,
    RetainedRoutineValue, RoutineBody, SQLRoutine, SQLUserFunction,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{
        Budgeted, BudgetedVec, MemoryBudget, MemoryError, OwnedMap, Produced, ProductionControl,
    },
    CancellationToken,
};

/// The native declarations remain in canonical-name and original overload order, independently of the registry's signature-sorted publication. Body finalization borrows these owners instead of retaining a second AST copy.
pub struct PendingRoutineDefinitions {
    groups: BudgetedVec<BudgetedVec<SQLRoutine>>,
}

impl PendingRoutineDefinitions {
    pub fn groups(&self) -> impl ExactSizeIterator<Item = &[SQLRoutine]> {
        self.groups.iter().map(|group| &**group)
    }
}

/// A fully prepared private namespace and the same declaration owners for subsequent body finalization. The receiving state owner publishes this registry atomically and retains its previous generation until finalization succeeds.
pub struct RoutineRestorePlaceholders {
    registry: RoutineRegistry,
    definitions: PendingRoutineDefinitions,
}

impl RoutineRestorePlaceholders {
    pub fn into_parts(self) -> (RoutineRegistry, PendingRoutineDefinitions) {
        (self.registry, self.definitions)
    }
}

impl CanonicalRoutineDefinitions {
    /// Build all placeholders before publication, retaining existing temporary routines and the original native declaration leases. No body is compiled; source and SQL-standard declarations receive the same empty bound placeholder as ordinary restoration.
    pub async fn prepare_placeholders_from_input(
        self,
        previous: &RoutineRegistry,
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<RoutineRestorePlaceholders, RoutineRestoreInputError> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        checkpoint(&control, input).await?;
        let mut registry = previous.temporary_only().into_mutation_budget(memory);
        let groups = self.into_groups();
        let mut pending = BudgetedVec::new(memory);
        pending.reserve(groups.len())?;
        for CanonicalRoutineGroup {
            name,
            definitions,
            temporary,
        } in groups
        {
            checkpoint(&control, input).await?;
            let count = definitions.len();
            let (original_order, sorted) = prepare_group(definitions, &control, input).await?;
            let mut overloads = BudgetedVec::new(memory);
            overloads.reserve(count)?;
            let mut children = 0usize;
            let mut sorted = sorted.into_iter();
            while let Some((signature, routine)) = sorted.next_retained() {
                checkpoint(&control, input).await?;
                let RoutineBody::Bound(body) = &routine.body else {
                    unreachable!("bound placeholder")
                };
                children = children
                    .checked_add(routine.reserved_bytes())
                    .and_then(|bytes| bytes.checked_add(body.reserved_bytes()))
                    .ok_or(MemoryError::SizeOverflow)?;
                overloads.push(routine)?;
                drop(signature);
            }
            let (overloads, lease) = overloads.into_parts();
            registry
                .insert_restored_group_from(
                    name.into_budgeted().expect("native canonical name"),
                    Budgeted::new(overloads, lease),
                    children,
                    temporary,
                    &mut Comparison { control, input },
                )
                .await?;
            pending.push(original_order)?;
        }
        checkpoint(&control, input).await?;
        Ok(RoutineRestorePlaceholders {
            registry,
            definitions: PendingRoutineDefinitions { groups: pending },
        })
    }
}

type OrderedRoutines = OwnedMap<(Vec<String>, bool), SQLRoutine>;

async fn prepare_group(
    definitions: BudgetedVec<Produced<crate::ast::CreateFunction>>,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(BudgetedVec<SQLRoutine>, OrderedRoutines), RoutineRestoreInputError> {
    let memory = control.budget().expect("native routine placeholders");
    let (definitions, lease) = definitions.into_parts();
    let result = Budgeted::new(definitions, lease)
        .try_map_async(async |definitions| {
            let mut original_order = BudgetedVec::new(memory);
            original_order.reserve(definitions.len())?;
            let mut sorted = OwnedMap::new();
            for definition in definitions {
                checkpoint(control, input).await?;
                assert!(
                    definition.is_retained_by(memory),
                    "native placeholder retains its declaration allowance"
                );
                let signature =
                    routine_signature_types_from_input(&definition, control, input).await?;
                let procedure = definition.is_procedure;
                let body = RetainedRoutineValue::native(
                    control.finish(
                        CompiledFunctionBody::SQL(Vec::new()),
                        control.empty_reservation(),
                    )?,
                    control,
                )?;
                let (definition, lease) = definition.into_parts();
                let routine = RetainedRoutineValue::native(
                    control.finish(
                        SQLUserFunction::new(definition, RoutineBody::Bound(body)),
                        lease,
                    )?,
                    control,
                )?;
                original_order.push(routine.clone())?;
                let (signature, lease) = signature.into_parts();
                let key = Budgeted::new((signature, procedure), lease.expect("native signature"));
                let replaced = sorted
                    .insert_admitted_key_from(
                        key,
                        routine,
                        &mut Comparison {
                            control: *control,
                            input,
                        },
                    )
                    .await?;
                assert!(replaced.is_none(), "canonical signatures are unique");
            }
            Ok::<_, RoutineRestoreInputError>((original_order, sorted))
        })
        .await?;
    let (result, lease) = result.into_parts();
    // The source vector has been consumed and destroyed inside the completed transformation.
    drop(lease);
    Ok(result)
}
