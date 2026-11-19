//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current schema catalogs retain physical input and ordered authority checks until native publication completes.

use super::{
    Identities, BUILTIN_INITIALIZED, BUILTIN_MIGRATION, IDENTITY_MIGRATION, SECURITY_MIGRATION,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, MemoryReservation, ProductionControl},
};
use uqa_sql::{
    catalog::{
        roles::RoleDefinitions,
        security::{schema_registry::SchemaRegistry, BoundSchemaSecurity},
    },
    SQLError,
};
use uqa_storage::{
    catalog::{input::CatalogInputLimits, schema::load::CatalogSchemaRow},
    read_control::StorageReadControl,
    CatalogFacade, SchemaRow, StorageBackendError, StorageBackendResult,
};

struct Rows {
    entries: std::vec::IntoIter<CatalogSchemaRow>,
    _memory: MemoryReservation,
}

pub struct SchemaCatalogInput {
    input: ImageDecoder<SchemaRegistry, StorageBackendError>,
}

impl SchemaCatalogInput {
    /// Restore only current records while the caller retains one complete read transaction. Initial-open migrations remain with ordinary restoration; no metadata is written and no partial root is returned.
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
                let mut reader = reader.input();
                let control = ProductionControl::new(
                    retained.memory(),
                    retained.cancellation(),
                    reader.cancellation,
                );
                reader.checkpoint().await?;
                let mut source = catalog.begin_schema_load(limits, &retained)?;
                let rows = loop {
                    reader.checkpoint().await?;
                    if let Some(rows) = source.advance(1)? {
                        break rows;
                    }
                };
                drop(source);
                let (entries, memory) = rows.into_parts();
                let mut rows = Rows {
                    entries: entries.into_iter(),
                    _memory: memory,
                };
                let mut values = SchemaRegistry::with_budget(retained.memory());
                let mut identities = Identities::new(control);
                for row in rows.entries.by_ref() {
                    reader.checkpoint().await?;
                    uqa_sql::schema::namespaces::validate_stored_schema_name(row.name())
                        .map_err(StorageBackendError::Other)?;
                    let row = row.into_budgeted().try_map(|row| match row {
                        SchemaRow::Bound(row) => Ok(row),
                        SchemaRow::Legacy(_) => {
                            Err(StorageBackendError::Other(SECURITY_MIGRATION.into()))
                        }
                    })?;
                    let (name, security) = BoundSchemaSecurity::from_budgeted_row(row);
                    security
                        .validate_from_input(&roles, &control, &mut reader)
                        .await
                        .map_err(sql_error)?;
                    let tuple = security
                        .tuple
                        .ok_or_else(|| StorageBackendError::Other(IDENTITY_MIGRATION.into()))?;
                    identities.check(&name, tuple)?;
                    if values.contains_key(name.as_str()) {
                        return Err(StorageBackendError::Other(format!(
                            "duplicate schema security for `{}`",
                            name.as_str()
                        )));
                    }
                    values
                        .insert_stored_entry(name, security, &control)
                        .map_err(sql_error)?;
                }
                drop(rows);
                reader.checkpoint().await?;
                if catalog
                    .get_metadata_with_control(BUILTIN_INITIALIZED, limits, &retained)?
                    .is_none()
                {
                    return Err(StorageBackendError::Other(BUILTIN_MIGRATION.into()));
                }
                control.check()?;
                Ok(values.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }

    /// Bound physical source slots, stable ordering, role visits and schema publication through native polls. Every terminal path retires all private fields, roots and scratch.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<SchemaRegistry>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

fn sql_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Cancelled(error) => error.into(),
        SQLError::Internal(message) => StorageBackendError::Other(message),
        error => StorageBackendError::backend("stored schema catalog", error),
    }
}

#[cfg(test)]
mod tests;
