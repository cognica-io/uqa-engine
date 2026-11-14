//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::canonical::RoutineRestoreSchemaInput;
use super::{RoutineCatalogDefinitions, Slots};
use std::cmp::Ordering;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MapKeyInput, MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    ast::TypeIdentityInputError,
    routines::lifecycle::restoration::{
        identify_routine_signature_types_from_input, persisted_registry_relation_from_input,
        validate_persisted_routine_schema_from_input, CanonicalRoutineDefinitions,
        RetainedRoutineRestoreBuilder, RoutineRestoreInputError, RoutineTypeBindingError,
    },
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

impl RoutineCatalogDefinitions {
    pub(in super::super) async fn canonicalize_from<R: RoutineRestoreSchemaInput>(
        self,
        names: &mut R,
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<CanonicalRoutineDefinitions> {
        // Only the load-only physical reader admits this transition; legacy migration keeps its separate original entry point.
        debug_assert_eq!(self.format, 3);
        let control = ProductionControl::new(memory, original, input.cancellation);
        let mut builder = RetainedRoutineRestoreBuilder::new(memory);
        for group in Slots::new(self.definitions) {
            super::super::checkpoint(&control, input).await?;
            let relation =
                persisted_registry_relation_from_input(&group.name, original, &control, input)
                    .await
                    .map_err(|error| restore_error(error, &control))?;
            let exists = names
                .schemas()
                .contains_key_from(relation.schema.as_str(), &mut Comparison { control, input })
                .await?;
            validate_persisted_routine_schema_from_input(
                &group.name,
                &relation,
                exists,
                &control,
                input,
            )
            .await
            .map_err(|error| restore_error(error, &control))?;
            for definition in Slots::new(group.overloads) {
                let definition =
                    identify_routine_signature_types_from_input(definition, names, &control, input)
                        .await
                        .map_err(|error| match error {
                            RoutineTypeBindingError::Input(error) => identity_error(error),
                            RoutineTypeBindingError::Lookup(error) => error,
                        })?;
                builder = builder
                    .insert_from_input(&group.name, &relation, definition, original, input)
                    .await
                    .map_err(|error| restore_error(error, &control))?;
            }
        }
        builder
            .finish_from_input(original, input)
            .await
            .map_err(|error| restore_error(error, &control))
    }
}

fn identity_error(error: TypeIdentityInputError) -> StorageBackendError {
    match error {
        TypeIdentityInputError::Native(error) => error.into(),
        TypeIdentityInputError::Work(error) => error.into(),
    }
}

fn restore_error(
    error: RoutineRestoreInputError,
    control: &ProductionControl<'_>,
) -> StorageBackendError {
    match error {
        RoutineRestoreInputError::Native(error) => error.into(),
        RoutineRestoreInputError::Work(error) => error.into(),
        RoutineRestoreInputError::Diagnostic(error) => match error.into_shared(control) {
            Ok(error) => StorageBackendError::RetainedDiagnostic(error),
            Err(error) => error.into(),
        },
    }
}

struct Comparison<'a, 'b, 'c> {
    control: ProductionControl<'a>,
    input: &'b mut AsyncImageInput<'c>,
}
impl MapKeyInput<str, String> for Comparison<'_, '_, '_> {
    type Error = StorageBackendError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        super::super::checkpoint(&self.control, self.input).await
    }
    async fn compare(&mut self, query: &str, stored: &String) -> Result<Ordering, Self::Error> {
        for (left, right) in query
            .as_bytes()
            .chunks(4096)
            .zip(stored.as_bytes().chunks(4096))
        {
            self.checkpoint().await?;
            let order = left.cmp(right);
            if order.is_ne() {
                return Ok(order);
            }
        }
        Ok(query.len().cmp(&stored.len()))
    }
}
