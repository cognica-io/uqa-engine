//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine and body images retire independently at the same lifetime as their native shared value.

use super::{
    super::{Kind as PlanKind, Owner},
    invalid, Empty, Fields, Input, Kind, Node, SQLError,
};
use crate::{
    ast::{CreateFunction, FunctionBody},
    catalog::{image::input::metadata::Identities, named_definitions::DefinitionImage},
    plan::UnifiedPlan,
    routines::{
        registry::RoutineRegistry, CompiledFunctionBody, RetainedRoutineValue, RoutineBody,
        SQLRoutine, SQLUserFunction, SharedRoutineBody,
    },
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError, MemoryReservation},
    CancellationToken, RelationIdentity,
};

mod fields;
#[cfg(test)]
mod tests;

struct RoutineImage {
    value: SQLUserFunction,
    definition: Owner<Node>,
}
impl Drop for RoutineImage {
    fn drop(&mut self) {
        let Node::Routine(definition) = &mut self.definition.root else {
            unreachable!("native routine definition owner")
        };
        std::mem::swap(definition, &mut self.value.def);
    }
}
impl DefinitionImage<SQLUserFunction> for RoutineImage {
    fn definition(&self) -> &SQLUserFunction {
        &self.value
    }
    fn budget(&self) -> &MemoryBudget {
        self.definition.memory.budget()
    }
    fn retained_bytes(&self) -> Result<usize, MemoryError> {
        self.definition.retained_bytes()
    }
}

struct BodyImage {
    value: CompiledFunctionBody,
    owners: Vec<Owner>,
    memory: MemoryReservation,
}
impl Drop for BodyImage {
    fn drop(&mut self) {
        let CompiledFunctionBody::SQL(plans) = &mut self.value else {
            unreachable!("stored SQL body")
        };
        for (plan, owner) in plans.iter_mut().zip(&mut self.owners) {
            owner.exchange_unified(plan);
        }
    }
}
impl DefinitionImage<CompiledFunctionBody> for BodyImage {
    fn definition(&self) -> &CompiledFunctionBody {
        &self.value
    }
    fn budget(&self) -> &MemoryBudget {
        self.memory.budget()
    }
    fn retained_bytes(&self) -> Result<usize, MemoryError> {
        self.owners
            .iter()
            .try_fold(self.memory.bytes(), |bytes, owner| {
                bytes
                    .checked_add(owner.retained_bytes()?)
                    .ok_or(MemoryError::SizeOverflow)
            })
    }
}

macro_rules! routine_input {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            $($async)? fn routine_bound_body(&mut self) -> Result<SharedRoutineBody, SQLError> {
                let count = $read!(self.binary.count())?;
                let mut plans = BudgetedVec::<UnifiedPlan>::new(self.binary.memory);
                let mut owners = BudgetedVec::<Owner>::new(self.binary.memory);
                plans.reserve(count)?;
                owners.reserve(count)?;
                let (plans, mut memory) = plans.into_parts();
                let (owners, lease) = owners.into_parts();
                memory.absorb(lease);
                let mut image = BodyImage {
                    value: CompiledFunctionBody::SQL(plans),
                    owners,
                    memory,
                };
                for _ in 0..count {
                    $read!(self.checkpoint())?;
                    let kind = match $read!(self.binary.array::<1>())?[0] {
                        0 => PlanKind::Query,
                        1 => PlanKind::Command,
                        _ => return Err(invalid("bound routine member kind")),
                    };
                    let mut owner = $read!(self.native_plan(kind))?;
                    let value = owner.unified_value()?;
                    let CompiledFunctionBody::SQL(plans) = &mut image.value else {
                        unreachable!("stored SQL body")
                    };
                    // Both vectors were admitted before descent. Publication cannot fail between these paired native moves.
                    plans.push(value);
                    image.owners.push(owner);
                }
                RetainedRoutineValue::image(image)
            }

            $($async)? fn routine(
                &mut self,
                name: &str,
                identities: &mut Identities,
            ) -> Result<SQLRoutine, SQLError> {
                let mut owner = $read!(self.native_tree::<Node>(Kind::Routine))?;
                let Node::Routine(definition) = &mut owner.root else {
                    unreachable!("native routine definition")
                };
                let definition = std::mem::replace(definition, <CreateFunction as Empty>::empty());
                let mut image = RoutineImage {
                    value: SQLUserFunction::new(definition, RoutineBody::Source),
                    definition: owner,
                };
                if $read!(self.compare_text(&image.value.def.name, name))?
                    != std::cmp::Ordering::Equal
                {
                    return Err(invalid("routine catalog key"));
                }
                if let Some(id) = image.value.def.object_id {
                    identities.object(id)?;
                }
                if let Some(oid) = image.value.def.catalog_oid {
                    if oid == 0 {
                        return Err(invalid("routine catalog OID"));
                    }
                    identities.oid(oid)?;
                }
                let bound = $read!(self.flag())?;
                if bound != matches!(image.value.def.body, FunctionBody::Statements(_)) {
                    return Err(invalid("routine source and bound body form"));
                }
                if bound {
                    image.value.body = RoutineBody::Bound($read!(self.routine_bound_body())?);
                }
                RetainedRoutineValue::image(image)
            }
            $($async)? fn routine_entry(
                &mut self,
                result: &mut RoutineRegistry,
                identities: &mut Identities,
            ) -> Result<(), SQLError> {
                let name = $read!(self.ordered_name(result.last_durable_key()))?;
                let relation = RelationIdentity::try_borrowed_reference(&name)
                    .map_err(|_| invalid("routine catalog name"))?;
                if relation
                    .schema
                    .is_some_and(crate::catalog::temporary_namespace::is_temporary_schema_reference)
                {
                    return Err(invalid("temporary routine catalog name"));
                }
                let overloads = $read!(self.binary.count())?;
                let mut values = BudgetedVec::new(self.binary.memory);
                values.reserve(overloads)?;
                for _ in 0..overloads {
                    $read!(self.checkpoint())?;
                    values.push($read!(self.routine(&name, identities))?)?;
                }
                let (values, lease) = values.into_parts();
                result.insert_image_entry(name, Budgeted::new(values, lease))
            }
        }
    };
}
routine_input!(Input, read_field);
routine_input!(Fields, await_field, async);

pub(in crate::catalog::image) fn read(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<RoutineRegistry, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(*b"UQAROU01")?;
    let mut result = RoutineRegistry::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        input.routine_entry(&mut result, &mut identities)?;
    }
    input.binary.end()?;
    Ok(result)
}

impl RoutineRegistry {
    /// Retain original routine declarations, source text, bound SQL plans and overload identity claims across independent entry, byte and poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<crate::catalog::image::CatalogImageInput<Self>, SQLError> {
        crate::catalog::image::input::continuation::registry(memory, mutations, cancellation)
    }
}
impl crate::catalog::image::input::continuation::CatalogEntries for RoutineRegistry {
    type Scratch = Identities;
    const MAGIC: [u8; 8] = *b"UQAROU01";
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), Identities::new(memory))
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        identities: &mut Identities,
    ) -> Result<(), SQLError> {
        await_field!(input.routine_entry(result, identities))
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
