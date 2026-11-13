//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Every typed body completes before ordered authority and object-identity validation.

use super::{checkpoint, RoutineCatalogAuthority, StorageBackendError, StorageBackendResult};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation, Produced, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    ast::CreateFunction, routines::security::binding::validate_routine_authority_from_input,
};

struct DefinitionGroup {
    name: Budgeted<String>,
    overloads: BudgetedVec<Produced<CreateFunction>>,
}

/// Complete typed syntax and original authority predicates. Empty ordered groups remain available to later name/schema binding; this owner publishes no routine registry.
pub struct RoutineCatalogDefinitions {
    format: u32,
    definitions: BudgetedVec<DefinitionGroup>,
}

impl RoutineCatalogDefinitions {
    pub fn requires_migration(&self) -> bool {
        self.format != 3
    }

    pub fn definitions(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &[Produced<CreateFunction>])> {
        self.definitions
            .iter()
            .map(|group| (group.name.as_str(), &*group.overloads))
    }
}

impl RoutineCatalogAuthority {
    /// Move prepared native authority into each outer declaration, finish all typed bodies, then validate against the same immutable role generation selected during raw authority preparation.
    pub async fn decode_from(
        self,
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<RoutineCatalogDefinitions> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        let protocol = crate::catalog::type_records::marker::protocol::keys(&control)?;
        let (catalog, legacy, roles) = self.into_parts();
        let format = catalog.format;
        let mut legacy = Slots::new(legacy);
        let mut definitions = BudgetedVec::new(memory);
        for group in Slots::new(catalog.definitions) {
            checkpoint(&control, input).await?;
            let mut overloads = BudgetedVec::new(memory);
            for definition in Slots::new(group.overloads) {
                checkpoint(&control, input).await?;
                let value = if format < 2 {
                    CreateFunction::from_projected_json_with_authority(
                        definition.root(),
                        legacy
                            .next()
                            .expect("prepared legacy authority for every definition"),
                        &control,
                        input,
                        protocol,
                    )
                    .await?
                } else {
                    CreateFunction::from_projected_json_with_work(
                        definition.root(),
                        &control,
                        input,
                        protocol,
                    )
                    .await?
                };
                overloads.push(value)?;
            }
            definitions.push(DefinitionGroup {
                name: group.name,
                overloads,
            })?;
        }
        assert!(legacy.next().is_none(), "all prepared authority consumed");
        drop(legacy);
        let output = RoutineCatalogDefinitions {
            format,
            definitions,
        };
        for (_, overloads) in output.definitions() {
            for definition in overloads {
                checkpoint(&control, input).await?;
                validate_routine_authority_from_input(definition, &roles, &control, input)
                    .await
                    .map_err(super::authority::binding_error)?;
                if format >= 2 {
                    if let Some(error) = super::super::encoding::missing_object_identity(definition)
                    {
                        return Err(identity_error(error.0, &control, input).await?);
                    }
                }
            }
        }
        control.check()?;
        Ok(output)
    }
}

struct Slots<T> {
    values: std::vec::IntoIter<T>,
    _memory: MemoryReservation,
}

impl<T> Slots<T> {
    fn new(values: BudgetedVec<T>) -> Self {
        let (values, memory) = values.into_parts();
        Self {
            values: values.into_iter(),
            _memory: memory,
        }
    }
}

impl<T> Iterator for Slots<T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next()
    }
}

#[derive(Debug)]
struct RetainedIdentityError {
    name: Budgeted<String>,
    _memory: MemoryReservation,
}

impl std::fmt::Display for RetainedIdentityError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(
            &super::super::encoding::MissingObjectIdentity(&self.name),
            output,
        )
    }
}

impl std::error::Error for RetainedIdentityError {}

async fn identity_error(
    name: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<StorageBackendError> {
    let name = super::copy_text(name, control, input).await?;
    let memory = control
        .reserve(size_of::<RetainedIdentityError>())?
        .expect("admitted routine catalog input");
    Ok(StorageBackendError::RetainedDiagnostic(
        std::sync::Arc::new(RetainedIdentityError {
            name,
            _memory: memory,
        }),
    ))
}

#[cfg(test)]
mod tests;
