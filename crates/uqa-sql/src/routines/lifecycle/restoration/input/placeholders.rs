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
    groups: BudgetedVec<PendingGroup>,
}

struct PendingGroup {
    original: BudgetedVec<SQLRoutine>,
    signature_order: BudgetedVec<usize>,
    temporary: bool,
}

impl PendingRoutineDefinitions {
    pub fn groups(&self) -> impl ExactSizeIterator<Item = &[SQLRoutine]> {
        self.groups.iter().map(|group| &*group.original)
    }

    pub fn group(&self, index: usize) -> Option<&[SQLRoutine]> {
        self.groups.get(index).map(|group| &*group.original)
    }

    /// Move completed bodies into the same signature order already established before binding.
    /// Body compilation cannot change a routine's declared signature or its procedure flag.
    pub async fn publish_compiled_group_from_input(
        &self,
        index: usize,
        compiled: BudgetedVec<SQLRoutine>,
        registry: &mut RoutineRegistry,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<(), RoutineRestoreInputError> {
        let group = &self.groups[index];
        assert_eq!(compiled.len(), group.original.len());
        let control = ProductionControl::new(compiled.budget(), original, input.cancellation);
        let name = super::text(&group.original[0].def.name, &control, input).await?;
        group
            .publish(name, &compiled, registry, &control, input)
            .await
    }
}

impl PendingGroup {
    async fn publish(
        &self,
        name: Produced<String>,
        routines: &[SQLRoutine],
        registry: &mut RoutineRegistry,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<(), RoutineRestoreInputError> {
        let mut overloads = BudgetedVec::new(control.budget().expect("native routine registry"));
        overloads.reserve(routines.len())?;
        let mut children = 0usize;
        for &index in self.signature_order.iter() {
            checkpoint(control, input).await?;
            let routine = &routines[index];
            children = children
                .checked_add(crate::routines::ownership::retained_bytes(routine)?)
                .ok_or(MemoryError::SizeOverflow)?;
            overloads.push(routine.clone())?;
        }
        let (overloads, lease) = overloads.into_parts();
        registry
            .insert_restored_group_from(
                name.into_budgeted().expect("native canonical name"),
                Budgeted::new(overloads, lease),
                children,
                self.temporary,
                &mut Comparison {
                    control: *control,
                    input,
                },
            )
            .await
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
            let mut signature_order = BudgetedVec::new(memory);
            signature_order.reserve(count)?;
            let mut sorted = sorted.into_iter();
            while let Some((signature, index)) = sorted.next_retained() {
                checkpoint(&control, input).await?;
                signature_order.push(index)?;
                drop(signature);
            }
            let group = PendingGroup {
                original: original_order,
                signature_order,
                temporary,
            };
            group
                .publish(name, &group.original, &mut registry, &control, input)
                .await?;
            pending.push(group)?;
        }
        checkpoint(&control, input).await?;
        Ok(RoutineRestorePlaceholders {
            registry,
            definitions: PendingRoutineDefinitions { groups: pending },
        })
    }
}

type OrderedRoutines = OwnedMap<(Vec<String>, bool), usize>;

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
                let index = original_order.len();
                original_order.push(routine)?;
                let (signature, lease) = signature.into_parts();
                let key = Budgeted::new((signature, procedure), lease.expect("native signature"));
                let replaced = sorted
                    .insert_admitted_key_from(
                        key,
                        index,
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
