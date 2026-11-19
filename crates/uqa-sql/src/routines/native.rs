//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concrete routine references retain their original identities without resolving new overloads.

use crate::{
    ast::{CreateFunction, FunctionBinding},
    catalog::work::{inline, retire_entries, ReferenceWork, Work},
    SQLError,
};
use uqa_core::{
    memory::{BudgetedMap, MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken,
};

mod bodies;
pub use bodies::{validate_captured_body, validate_captured_body_with_work};
mod runtime_calls;
pub use runtime_calls::visit_stored_routine_runtime_calls;
mod bindings;
pub use bindings::{validate_captured_bindings, validate_captured_bindings_with_work};
mod references;
pub use references::StoredRoutineReference;
mod trigger_names;

/// An admitted borrowed identity index for one immutable routine registry. Late-bound calls and deferred source bodies retain their existing execution contract; runtime resource availability is checked separately.
pub struct NativeRoutineReferences<'a> {
    definitions: BudgetedMap<[u8; 16], &'a CreateFunction>,
    trigger_names: OwnedMap<trigger_names::Name<'a>, &'a CreateFunction>,
    control: ProductionControl<'a>,
}

impl<'a> NativeRoutineReferences<'a> {
    pub fn new(
        definitions: impl Iterator<Item = &'a CreateFunction>,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<Self, SQLError> {
        inline(Self::from_groups_with_work(
            std::iter::once(definitions),
            memory,
            cancellation,
            &mut (),
        ))
    }

    pub async fn from_groups_with_work<W: Work + Send, I: Iterator<Item = &'a CreateFunction>>(
        groups: impl Iterator<Item = I>,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
        work: &mut W,
    ) -> Result<Self, SQLError> {
        let control = ProductionControl::new(memory, cancellation, cancellation);
        control.check()?;
        let mut registry = BudgetedMap::new(memory);
        let mut trigger_names = OwnedMap::new();
        let mut input = ReferenceWork {
            control: &control,
            work,
        };
        for definitions in groups {
            input.checkpoint().await?;
            'routine: for definition in definitions {
                input.checkpoint().await?;
                let identity = definition
                    .object_id
                    .ok_or_else(|| invalid("missing identity"))?;
                if identity == [0; 16] || registry.insert(identity, definition)?.is_some() {
                    return Err(invalid("duplicate or zero identity"));
                }
                if definition.is_procedure {
                    continue;
                }
                for parameter in &definition.params {
                    input.checkpoint().await?;
                    if CreateFunction::is_identity_param(parameter) {
                        continue 'routine;
                    }
                }
                let name = trigger_names::name(&definition.name, &mut input).await?;
                let key = uqa_core::memory::Budgeted::new(name, memory.reserve(0)?);
                if trigger_names
                    .insert_admitted_key_from(key, definition, &mut input)
                    .await?
                    .is_some()
                {
                    return Err(invalid("duplicate zero-argument function name"));
                }
            }
        }
        Ok(Self {
            definitions: registry,
            trigger_names,
            control,
        })
    }

    /// Retire the original borrowed registry entries in the same operation that validated them.
    pub async fn retire_with_work(self, work: &mut impl Work) -> Result<(), SQLError> {
        retire_entries(self.definitions.into_entries(), &self.control, work).await?;
        retire_entries(self.trigger_names.into_iter(), &self.control, work).await
    }

    /// Validate a selected function identity and its original declared signature. Display names can be historical after a rename; the ordinary lookup contract selects a concrete identity independently of that spelling.
    pub fn binding(&self, binding: &FunctionBinding) -> Result<(), SQLError> {
        inline(self.binding_with_work(binding, &mut ()))
    }

    pub async fn binding_with_work(
        &self,
        binding: &FunctionBinding,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        self.control.check()?;
        if binding.builtin || binding.dispatch.is_some() || binding.resolution_error.is_some() {
            return Ok(());
        }
        if let Some(identity) = binding.object_id {
            self.identity_with_work(identity, &binding.argument_types, false, work)
                .await?;
        }
        Ok(())
    }

    pub fn identity(
        &self,
        identity: [u8; 16],
        argument_types: &[String],
        procedure: bool,
    ) -> Result<(), SQLError> {
        inline(self.identity_with_work(identity, argument_types, procedure, &mut ()))
    }

    pub async fn identity_with_work(
        &self,
        identity: [u8; 16],
        argument_types: &[String],
        procedure: bool,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        self.definition(identity, argument_types, procedure, work)
            .await
            .map(|_| ())
    }

    /// A stored trigger selects its original identity or, for legacy entries, its exact qualified name. Both paths require the ordinary trigger contract without compiling the body or changing stored metadata.
    pub fn trigger(&self, name: &str, identity: Option<[u8; 16]>) -> Result<(), SQLError> {
        inline(self.trigger_with_work(name, identity, &mut ()))
    }

    pub async fn trigger_with_work(
        &self,
        name: &str,
        identity: Option<[u8; 16]>,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        let definition = if let Some(id) = identity {
            self.definition(id, &[], false, work).await?
        } else {
            let mut input = ReferenceWork {
                control: &self.control,
                work,
            };
            let key = trigger_names::name(name, &mut input).await?;
            self.trigger_names
                .get_from(&key, &mut input)
                .await?
                .ok_or_else(|| invalid("missing legacy trigger function"))?
        };
        crate::catalog::events::validation::validate_trigger_function_with_work(
            definition,
            &self.control,
            work,
        )
        .await
    }

    async fn definition(
        &self,
        identity: [u8; 16],
        argument_types: &[String],
        procedure: bool,
        work: &mut (impl Work + Send),
    ) -> Result<&CreateFunction, SQLError> {
        self.control.check()?;
        let definition = self
            .definitions
            .get(&identity)
            .ok_or_else(|| invalid("missing bound identity"))?;
        if definition.is_procedure != procedure
            || !super::signature_matches(definition, argument_types, &self.control, work).await?
        {
            return Err(invalid("bound signature or kind"));
        }
        Ok(definition)
    }
}

fn invalid(detail: &str) -> SQLError {
    SQLError::Internal(format!("invalid native routine reference: {detail}"))
}

#[cfg(test)]
mod tests;
