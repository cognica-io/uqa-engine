//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical records and claims precede the native type owner's semantic validation.

use super::{TypeRecord, TypeRecordFormat};
pub(crate) use crate::catalog::text_records::{claim_oid, rows};
use std::{future::Future, sync::Arc};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget, Produced, ProductionControl, ProductionMap},
};
use uqa_sql::{
    catalog::{
        roles::RoleDefinitions,
        type_definitions::{insert_stored_entry, TypeDefinitions},
    },
    SQLError,
};
use uqa_storage::{
    catalog::{input::CatalogInputLimits, text_input::CatalogTextRow},
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendError, StorageBackendResult,
};

mod kinds;
#[cfg(test)]
mod tests;

pub struct TypeCatalogInput<T> {
    input: ImageDecoder<TypeDefinitions<T>, StorageBackendError>,
}
impl<T> TypeCatalogInput<T> {
    /// A failed, abandoned or completed continuation cannot publish or resume a partial registry.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<TypeDefinitions<T>>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

pub(crate) trait InputType: TypeRecord + Send + Sync + 'static {
    fn decode(text: &str, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError>;
    fn oid_count(&self) -> usize;
    fn oid_at(&self, index: usize) -> u32;
    fn validate(
        registry: &TypeDefinitions<Self>,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        work: &mut AsyncImageInput<'_>,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

pub(crate) fn new<T: InputType>(
    format: &'static TypeRecordFormat,
    catalog: Arc<dyn CatalogFacade>,
    roles: RoleDefinitions,
    limits: CatalogInputLimits,
    mutations: &MemoryBudget,
    control: &StorageReadControl,
) -> StorageBackendResult<TypeCatalogInput<T>> {
    control.check()?;
    limits.validate()?;
    let retained = control.clone();
    let mutations = mutations.clone();
    let input = ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut work = reader.input();
            let production = ProductionControl::new(
                retained.memory(),
                retained.cancellation(),
                work.cancellation,
            );
            let definitions = rows(
                catalog.as_ref(),
                format.prefix,
                limits,
                &retained,
                &mut work,
            )
            .await?;
            let claims = rows(
                catalog.as_ref(),
                format.oid_prefix,
                limits,
                &retained,
                &mut work,
            )
            .await?;
            work.checkpoint().await?;
            let marker = catalog.get_metadata_with_control(format.format_key, limits, &retained)?;
            match marker {
                Some(marker) => {
                    super::marker::check_from(format, &marker, &production, &mut work).await?;
                }
                None if definitions.is_empty() && claims.is_empty() => {
                    return Ok(TypeDefinitions::with_budget(&mutations))
                }
                None => {
                    return Err(TypeRecordFormat::corrupt(format!(
                        "{} records exist without their format marker",
                        format.label
                    )))
                }
            }
            let mut registry = TypeDefinitions::with_budget(retained.memory());
            let (definitions, definition_memory) = definitions.into_parts();
            for row in definitions {
                work.checkpoint().await?;
                production.check()?;
                let name = row.name.strip_prefix(format.prefix).ok_or_else(|| {
                    TypeRecordFormat::corrupt(format!("{} record key {}", format.label, row.name))
                })?;
                let name = work.copy_text(name).await?;
                let definition = T::decode(&row.value, &production)
                    .map_err(decode_error)?
                    .into_budgeted()
                    .unwrap_or_else(|_| unreachable!("stored type input is admitted"));
                insert_stored_entry(&mut registry, name, definition)
                    .map_err(|error| sql_error(format, error))?;
            }
            drop(definition_memory);
            check_claims(format, &registry, &claims, &production, &mut work).await?;
            drop(claims);
            T::validate(&registry, &roles, &production, &mut work)
                .await
                .map_err(|error| match error {
                    SQLError::Internal(message) => StorageBackendError::Other(message),
                    error => sql_error(format, error),
                })?;
            retained.check()?;
            Ok(registry.into_mutation_budget(&mutations))
        },
    )?;
    Ok(TypeCatalogInput { input })
}

async fn check_claims<T: InputType>(
    format: &TypeRecordFormat,
    registry: &TypeDefinitions<T>,
    rows: &[Budgeted<CatalogTextRow>],
    control: &ProductionControl<'_>,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    let mismatch = || {
        TypeRecordFormat::corrupt(format!(
            "{} OID records do not match {} definitions",
            format.label, format.label
        ))
    };
    let mut expected = ProductionMap::new(*control);
    for (name, definition) in registry.iter() {
        work.checkpoint().await?;
        for index in 0..definition.oid_count() {
            work.checkpoint().await?;
            expected.insert_copy(definition.oid_at(index), name.as_str())?;
        }
    }
    let mut actual = ProductionMap::new(*control);
    for row in rows {
        work.checkpoint().await?;
        let text = row
            .name
            .strip_prefix(format.oid_prefix)
            .ok_or_else(mismatch)?;
        let oid = claim_oid(text).ok_or_else(mismatch)?;
        actual.insert_copy(oid, row.value.as_str())?;
    }
    if actual.len() != expected.len() {
        return Err(mismatch());
    }
    for (oid, name) in &expected {
        work.checkpoint().await?;
        let actual = actual.get(oid).ok_or_else(mismatch)?;
        if name.len() != actual.len() {
            return Err(mismatch());
        }
        for (expected, actual) in name
            .as_bytes()
            .chunks(4096)
            .zip(actual.as_bytes().chunks(4096))
        {
            work.checkpoint().await?;
            control.check()?;
            if expected != actual {
                return Err(mismatch());
            }
        }
    }
    Ok(())
}

pub(crate) fn decode_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Internal(message) => {
            StorageBackendError::Serde(<serde_json::Error as serde::de::Error>::custom(message))
        }
        SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("stored type input", error),
    }
}
fn sql_error(format: &TypeRecordFormat, error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend(format.definition_context, error),
    }
}
