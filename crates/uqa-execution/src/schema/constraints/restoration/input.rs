//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical catalogs precede ordered declaration validation and global identity claims.

use super::{addresses, claims};
use std::sync::Arc;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec, MemoryReservation, ProductionControl},
};
use uqa_sql::{
    ast::{ColumnDef, TableConstraintSet},
    schema::constraint_metadata::identity,
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
    StorageBackendError, StorageBackendResult,
};

struct Rows<T> {
    values: std::vec::IntoIter<T>,
    _memory: MemoryReservation,
}

impl<T> From<BudgetedVec<T>> for Rows<T> {
    fn from(values: BudgetedVec<T>) -> Self {
        let (values, memory) = values.into_parts();
        Self {
            values: values.into_iter(),
            _memory: memory,
        }
    }
}

pub struct ConstraintCatalogInput {
    input: ImageDecoder<(), StorageBackendError>,
}

impl ConstraintCatalogInput {
    /// Validate the caller's pinned catalog without migration or publication. Definitions and independent ACLs complete before semantic checks; ordinary declarations precede the foreign reference marker and foreign declarations. Record decoding retains the selected physical byte limit.
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut reader = reader.input();
                formats(catalog.as_ref(), limits, &retained, &mut reader).await?;
                let names =
                    crate::schema::indexes::constraint_names::KeyConstraintNames::load_from(
                        catalog.as_ref(),
                        limits,
                        &retained,
                        &mut reader,
                    )
                    .await?;
                let production = ProductionControl::new(
                    retained.memory(),
                    retained.cancellation(),
                    reader.cancellation,
                );
                let mut claims = claims::GlobalClaims::default();
                reader.checkpoint().await?;
                let mut source = catalog.begin_table_load(limits, &retained)?;
                let mut rows = loop {
                    reader.checkpoint().await?;
                    if let Some(rows) = source.advance(1)? {
                        break Rows::from(rows);
                    }
                };
                drop(source);
                for row in rows.values.by_ref() {
                    reader.checkpoint().await?;
                    let columns = uqa_sql::ast::read_stored_columns_json(
                        if row.columns_json.is_empty() {
                            "[]"
                        } else {
                            &row.columns_json
                        },
                        retained.memory(),
                        retained.cancellation(),
                        reader.cancellation,
                    )
                    .map_err(|error| sql_error("stored table columns", error))?;
                    let constraints = names.decode_with_control(
                        &row,
                        retained.memory(),
                        retained.cancellation(),
                        reader.cancellation,
                    )?;
                    if uqa_sql::schema::dependencies::rewrites::has_legacy_schema_function_dispatches_with_control(
                        &columns, &constraints, &production,
                    ).map_err(|error| sql_error("stored table expressions", error))? {
                        return Err(StorageBackendError::Other("table expressions require an initial-open migration".into()));
                    }
                    validate(
                        &columns,
                        &constraints,
                        &mut claims,
                        &production,
                        &mut reader,
                    )
                    .await?;
                }
                drop(rows);
                drop(names);
                foreign(
                    catalog.as_ref(),
                    limits,
                    &retained,
                    &mut claims,
                    &mut reader,
                )
                .await?;
                production.check()?;
                Ok(())
            },
        )?;
        Ok(Self { input })
    }

    /// Bound provider polls, declaration visits and identity claims. Completion and every failure retire the private continuation; a failed or completed input cannot resume.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<()>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn foreign(
    catalog: &dyn CatalogFacade,
    limits: CatalogInputLimits,
    retained: &StorageReadControl,
    claims: &mut claims::GlobalClaims,
    reader: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    let production = ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        reader.cancellation,
    );
    reader.checkpoint().await?;
    let reference = crate::catalog::foreign::reference::check_format_with_control(
        catalog,
        limits,
        retained,
        reader.cancellation,
    )?;
    reader.checkpoint().await?;
    let mut source = catalog.begin_foreign_table_load(limits, retained)?;
    let mut rows = loop {
        reader.checkpoint().await?;
        if let Some(rows) = source.advance(1)? {
            break Rows::from(rows);
        }
    };
    drop(source);
    for row in rows.values.by_ref() {
        reader.checkpoint().await?;
        let ((mut table, _), memory) =
            crate::catalog::foreign::StoredForeignTable::from_catalog_row_with_control(
                &row,
                reference,
                retained,
                reader.cancellation,
            )?
            .into_parts();
        let constraints = TableConstraintSet {
            checks: std::mem::take(&mut table.checks),
            dropped_attributes: std::mem::take(&mut table.dropped_attributes),
            ..Default::default()
        };
        // The remaining foreign fields and transferred constraints keep the original lease together.
        let declaration = Budgeted::new((table, constraints), memory);
        validate(
            &declaration.0.columns,
            &declaration.1,
            claims,
            &production,
            reader,
        )
        .await?;
    }
    Ok(())
}

async fn formats(
    catalog: &dyn CatalogFacade,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    reader: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    reader.checkpoint().await?;
    let marker =
        catalog.get_metadata_with_control(super::IDENTITY_METADATA_KEY, limits, control)?;
    super::not_null_format(marker.as_ref().map(|value| value.as_str()), false)?;
    drop(marker);
    reader.checkpoint().await?;
    let marker = catalog.get_metadata_with_control(
        super::FOREIGN_KEY_IDENTITY_METADATA_KEY,
        limits,
        control,
    )?;
    super::foreign_key_format(marker.as_ref().map(|value| value.as_str()), false)?;
    drop(marker);
    reader.checkpoint().await?;
    let marker =
        catalog.get_metadata_with_control(super::CATALOG_ADDRESS_METADATA_KEY, limits, control)?;
    addresses::format(marker.as_ref().map(|value| value.as_str()), false)?;
    drop(marker);
    reader.checkpoint().await?;
    let marker = catalog.get_metadata_with_control(
        super::RELATION_ATTRIBUTE_METADATA_KEY,
        limits,
        control,
    )?;
    super::attribute_format(marker.as_ref().map(|value| value.as_str()), false)?;
    Ok(())
}

async fn validate(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
    claims: &mut claims::GlobalClaims,
    control: &ProductionControl<'_>,
    reader: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    uqa_sql::catalog::relation_attributes::validate_from_input(
        columns,
        &constraints.dropped_attributes,
        control,
        reader,
    )
    .await
    .map_err(|error| sql_error("relation attributes", error))?;
    identity::validate_not_null_identities_from_input(columns, control, reader)
        .await
        .map_err(|error| identity_error(error, None))?;
    identity::foreign_keys::validate_from_input(columns, constraints, control, reader)
        .await
        .map_err(|error| identity_error(error, None))?;
    identity::claims::validate_key_and_check_identities_from_input(
        columns,
        constraints,
        control,
        reader,
    )
    .await
    .map_err(|error| identity_error(error, Some("constraint identity")))?;
    for (identity, kind) in claims::slots(columns, constraints) {
        reader.checkpoint().await?;
        claims.register(identity, kind, control)?;
    }
    Ok(())
}

fn sql_error(backend: &'static str, error: uqa_sql::SQLError) -> StorageBackendError {
    match error {
        uqa_sql::SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend(backend, error),
    }
}

fn identity_error(
    error: uqa_sql::schema::constraint_metadata::ConstraintMetadataError,
    backend: Option<&'static str>,
) -> StorageBackendError {
    match error {
        uqa_sql::schema::constraint_metadata::ConstraintMetadataError::Execution(error) => {
            sql_error(backend.unwrap_or("constraint identity"), *error)
        }
        error @ uqa_sql::schema::constraint_metadata::ConstraintMetadataError::Invalid(_) => {
            match backend {
                Some(backend) => StorageBackendError::backend(backend, error),
                None => StorageBackendError::Other(error.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests;
