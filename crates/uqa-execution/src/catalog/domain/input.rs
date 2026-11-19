//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current domain records retain their original marker, physical, claim and complete validation order.

use super::{
    records::{OID_PREFIX, PREFIX},
    restoration::invalid,
    DomainRegistry, RestoredDomains, DOMAINS_METADATA_KEY,
};
use crate::catalog::type_records::{
    input::{claim_oid, decode_error, rows},
    marker,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
};
use uqa_sql::{
    catalog::{
        domain::{
            validation::{validate_from_input, DomainValidationMode},
            StoredDomain,
        },
        roles::RoleDefinitions,
        type_definitions::insert_stored_entry,
    },
    schema::constraint_metadata::ConstraintMetadataError,
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
    StorageBackendError, StorageBackendResult,
};

pub struct DomainCatalogInput {
    input: ImageDecoder<RestoredDomains, StorageBackendError>,
}

impl DomainCatalogInput {
    /// Reconstruct current records under one selected catalog transaction; legacy migration stays at initial open.
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        roles: RoleDefinitions,
        limits: CatalogInputLimits,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
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
                current_marker(catalog.as_ref(), limits, &retained, &production, &mut work).await?;
                let definitions =
                    rows(catalog.as_ref(), PREFIX, limits, &retained, &mut work).await?;
                let mut registry = DomainRegistry::with_budget(retained.memory());
                let (definitions, definition_memory) = definitions.into_parts();
                for row in definitions {
                    work.checkpoint().await?;
                    production.check()?;
                    let name = row
                        .name
                        .strip_prefix(PREFIX)
                        .expect("selected domain record prefix");
                    let name = work.copy_text(name).await?;
                    let definition = StoredDomain::from_stored_schema_json(
                        &row.value,
                        retained.memory(),
                        retained.cancellation(),
                        work.cancellation,
                    )
                    .map_err(decode_error)?
                    .into_budgeted()
                    .unwrap_or_else(|_| unreachable!("stored domain input is admitted"));
                    insert_stored_entry(&mut registry, name, definition).map_err(|error| {
                        StorageBackendError::backend("domain definition", error)
                    })?;
                }
                drop(definition_memory);
                claims(catalog.as_ref(), &registry, limits, &retained, &mut work).await?;
                validate_from_input(
                    &registry,
                    &roles,
                    DomainValidationMode::Complete,
                    &production,
                    &mut work,
                )
                .await
                .map_err(|error| match error {
                    ConstraintMetadataError::Invalid(message) => invalid(message),
                    ConstraintMetadataError::Execution(error) => {
                        StorageBackendError::backend("domain constraint catalog", *error)
                    }
                })?;
                retained.check()?;
                Ok(RestoredDomains::current(
                    registry.into_mutation_budget(&mutations),
                ))
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<RestoredDomains>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn current_marker(
    catalog: &dyn CatalogFacade,
    limits: CatalogInputLimits,
    retained: &StorageReadControl,
    control: &ProductionControl<'_>,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    work.checkpoint().await?;
    let marker = catalog.get_metadata_with_control(DOMAINS_METADATA_KEY, limits, retained)?;
    let selected = match marker {
        Some(ref text) => marker::field_from(text, "domain_catalog_format", control, work).await?,
        None => marker::FieldProjection::default(),
    };
    if matches!(selected.unsigned, Some(2 | 3)) {
        if selected.foreign {
            return Err(StorageBackendError::Serde(
                <serde_json::Error as serde::de::Error>::custom(
                    "unknown domain catalog format field",
                ),
            ));
        }
        return if selected.unsigned == Some(3) {
            Ok(())
        } else {
            Err(StorageBackendError::Other(
                "domain constraints require initial catalog migration".into(),
            ))
        };
    }
    if !rows(catalog, PREFIX, limits, retained, work)
        .await?
        .is_empty()
        || !rows(catalog, OID_PREFIX, limits, retained, work)
            .await?
            .is_empty()
    {
        return Err(StorageBackendError::Other(
            "domain records exist without their format marker".into(),
        ));
    }
    Err(StorageBackendError::Other(
        "domain authority requires initial catalog migration".into(),
    ))
}

async fn claims(
    catalog: &dyn CatalogFacade,
    registry: &DomainRegistry,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    let claims = rows(catalog, OID_PREFIX, limits, control, work).await?;
    let mismatch =
        || StorageBackendError::Other("domain OID records do not match domain definitions".into());
    if claims.len() != registry.len() {
        return Err(mismatch());
    }
    for row in &*claims {
        work.checkpoint().await?;
        control.check()?;
        let definition = registry.get(row.value.as_str()).ok_or_else(mismatch)?;
        if row.name.strip_prefix(OID_PREFIX).and_then(claim_oid) != Some(definition.oid) {
            return Err(mismatch());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
