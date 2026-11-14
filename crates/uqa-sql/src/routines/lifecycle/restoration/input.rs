//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private load-only canonicalization retains native declarations and the original validation order.

use crate::ast::CreateFunction;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{BudgetedVec, MemoryBudget, OwnedMap, OwnedMapIntoIter, Produced, ProductionControl},
    CancellationToken, RelationIdentity, RelationName, RelationReferenceInputError,
};

mod diagnostics;
mod placeholders;
mod work;
pub use diagnostics::{RoutineRestoreDiagnostic, RoutineRestoreInputError};
pub use placeholders::{PendingRoutineDefinitions, RoutineRestorePlaceholders};
use work::{checkpoint, compare, message, text, Comparison};

struct Group {
    definitions: BudgetedVec<Produced<CreateFunction>>,
    temporary: bool,
}

/// Canonical names and ordered original declarations. This owner does not publish placeholders or compile bodies.
pub struct CanonicalRoutineDefinitions {
    definitions: OwnedMap<String, Group>,
}

impl CanonicalRoutineDefinitions {
    pub fn definitions(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &[Produced<CreateFunction>])> {
        self.definitions
            .iter()
            .map(|(name, group)| (name.as_str(), &*group.definitions))
    }

    /// Transfer canonical groups to the next native restoration owner without copying names, overload vectors or declarations. Completed output remains usable after the input operation retires.
    pub fn into_groups(self) -> CanonicalRoutineGroups {
        CanonicalRoutineGroups {
            definitions: self.definitions.into_iter(),
        }
    }
}

pub struct CanonicalRoutineGroup {
    pub name: Produced<String>,
    pub definitions: BudgetedVec<Produced<CreateFunction>>,
    temporary: bool,
}

/// Consuming group traversal keeps the original key and overload leases until their next owner retires them.
pub struct CanonicalRoutineGroups {
    definitions: OwnedMapIntoIter<String, Group>,
}
impl Iterator for CanonicalRoutineGroups {
    type Item = CanonicalRoutineGroup;

    fn next(&mut self) -> Option<Self::Item> {
        self.definitions
            .next_retained()
            .map(|(name, group)| CanonicalRoutineGroup {
                name,
                definitions: group.definitions,
                temporary: group.temporary,
            })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.definitions.size_hint()
    }
}
impl ExactSizeIterator for CanonicalRoutineGroups {}
impl std::iter::FusedIterator for CanonicalRoutineGroups {}

/// Insertion consumes the private candidate: an error or abandoned future retires it instead of leaving a partially usable registry.
pub struct RetainedRoutineRestoreBuilder {
    definitions: OwnedMap<String, Group>,
    object_ids: OwnedMap<[u8; 16], ()>,
    memory: MemoryBudget,
    migrated: bool,
}

impl RetainedRoutineRestoreBuilder {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            definitions: OwnedMap::new(),
            object_ids: OwnedMap::new(),
            memory: memory.clone(),
            migrated: false,
        }
    }

    pub async fn insert_from_input(
        mut self,
        stored_name: &str,
        stored_relation: &RelationIdentity,
        definition: Produced<CreateFunction>,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, RoutineRestoreInputError> {
        let memory = self.memory.clone();
        let control = ProductionControl::new(&memory, original, input.cancellation);
        checkpoint(&control, input).await?;
        assert!(
            definition.is_retained_by(&memory),
            "native routine declaration keeps its original allowance"
        );
        let Some(object_id) = definition.object_id else {
            return Err(message(
                &[
                    "persisted routine `",
                    stored_name,
                    "` has no object identity",
                ],
                &control,
                input,
            )
            .await?);
        };
        // Object keys have sixteen bytes and the AVL path has a fixed machine-word height bound; no input-sized comparison is hidden in this insertion.
        if self
            .object_ids
            .insert_budgeted(object_id, (), &memory)?
            .is_some()
        {
            return Err(message(
                &[
                    "duplicate persisted routine object identity for `",
                    stored_name,
                    "`",
                ],
                &control,
                input,
            )
            .await?);
        }
        self.migrated |= definition
            .has_legacy_serialized_dispatches_from_input(original, input)
            .await?;
        let canonical = canonical_name(
            &definition,
            stored_name,
            stored_relation,
            original,
            &control,
            input,
        )
        .await?;
        let signature =
            crate::routines::routine_signature_types_from_input(&definition, &control, input)
                .await?;
        let mut comparison = Comparison { control, input };
        if let Some(group) = self
            .definitions
            .get_mut_from(canonical.as_str(), &mut comparison)
            .await?
        {
            for existing in group.definitions.iter() {
                let existing = crate::routines::routine_signature_types_from_input(
                    existing,
                    &control,
                    comparison.input,
                )
                .await?;
                if work::signature_eq(&existing, &signature, &control, comparison.input).await? {
                    return Err(RoutineRestoreDiagnostic::duplicate(canonical, signature).into());
                }
            }
            group
                .definitions
                .push(rename(definition, canonical, &control)?)?;
        } else {
            let name = text(&canonical, &control, comparison.input).await?;
            let mut definitions = BudgetedVec::new(&memory);
            definitions.push(rename(definition, name, &control)?)?;
            self.definitions
                .insert_admitted_key_from(
                    canonical.into_budgeted().expect("admitted canonical name"),
                    Group {
                        definitions,
                        temporary: crate::catalog::temporary_namespace::is_temporary_schema_name(
                            &stored_relation.schema,
                        ),
                    },
                    &mut comparison,
                )
                .await?;
        }
        checkpoint(&control, comparison.input).await?;
        Ok(self)
    }

    /// Finish complete name/signature validation before rejecting a required initial-open migration. Source bodies have not been compiled and no native registry has been published.
    pub async fn finish_from_input(
        self,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<CanonicalRoutineDefinitions, RoutineRestoreInputError> {
        let memory = self.memory.clone();
        let control = ProductionControl::new(&memory, original, input.cancellation);
        checkpoint(&control, input).await?;
        if self.migrated {
            return Err(message(&["routine catalog requires an initial-open expression, object-identity or ACL format migration"], &control, input).await?);
        }
        control.check()?;
        Ok(CanonicalRoutineDefinitions {
            definitions: self.definitions,
        })
    }
}

async fn canonical_name(
    definition: &CreateFunction,
    stored_name: &str,
    stored_relation: &RelationIdentity,
    original: &CancellationToken,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Produced<String>, RoutineRestoreInputError> {
    let relation = relation(&definition.name, false, original, control, input).await?;
    if compare(&relation.schema, &stored_relation.schema, control, input)
        .await?
        .is_ne()
        || compare(&relation.name, &stored_relation.name, control, input)
            .await?
            .is_ne()
    {
        return Err(message(
            &[
                "persisted routine registry key `",
                stored_name,
                "` does not match definition `",
                &definition.name,
                "`",
            ],
            control,
            input,
        )
        .await?);
    }
    drop(relation);
    Ok(stored_relation
        .qualified_name_from_input(control, input)
        .await?)
}

/// Apply the native replacement at its owning declaration: retire the previous name allocation before releasing only its lease, then absorb the admitted canonical buffer.
fn rename(
    definition: Produced<CreateFunction>,
    name: Produced<String>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CreateFunction>, RoutineRestoreInputError> {
    let (mut definition, mut memory) = definition.into_parts();
    let (name, name_memory) = name.into_parts();
    let previous = std::mem::replace(&mut definition.name, name);
    let previous_capacity = previous.capacity();
    drop(previous);
    drop(
        memory
            .as_mut()
            .expect("admitted declaration")
            .split(previous_capacity),
    );
    Ok(control.finish(definition, control.combine(memory, name_memory))?)
}

pub async fn persisted_registry_relation_from_input(
    name: &str,
    original: &CancellationToken,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Produced<RelationIdentity>, RoutineRestoreInputError> {
    relation(name, true, original, control, input).await
}

async fn relation(
    name: &str,
    registry: bool,
    original: &CancellationToken,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Produced<RelationIdentity>, RoutineRestoreInputError> {
    checkpoint(control, input).await?;
    let reference =
        match RelationIdentity::try_borrowed_reference_from_input(name, original, input).await {
            Ok(reference) => reference,
            Err(RelationReferenceInputError::Input(error)) => return Err(error.into()),
            Err(RelationReferenceInputError::Reference(error)) => {
                let [first, second, third] = error.message_parts();
                return Err(message(
                    &[
                        if registry {
                            "invalid persisted routine registry key `"
                        } else {
                            "invalid persisted routine definition name `"
                        },
                        name,
                        "`: ",
                        first,
                        second,
                        third,
                    ],
                    control,
                    input,
                )
                .await?);
            }
        };
    Ok(reference
        .identity_from_input(RelationName::decoded("public"), control, input)
        .await?)
}

pub async fn validate_persisted_routine_schema_from_input(
    name: &str,
    relation: &RelationIdentity,
    exists: bool,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), RoutineRestoreInputError> {
    checkpoint(control, input).await?;
    if !exists {
        return Err(message(
            &[
                "persisted routine `",
                name,
                "` references missing schema `",
                &relation.schema,
                "`",
            ],
            control,
            input,
        )
        .await?);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
