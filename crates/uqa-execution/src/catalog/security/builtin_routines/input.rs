//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current builtin ACL records retain physical order, complete grant validation and native publication.

use super::{BuiltinRoutineSecurities, METADATA_PREFIX};
use crate::catalog::text_records::{claim_oid, rows};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget, ProductionControl},
};
use uqa_sql::{
    catalog::{
        roles::RoleDefinitions,
        security::builtin_routines::{
            stored::{insert_stored_entry, StoredSecurity},
            validate_from_input,
        },
    },
    SQLError,
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
    StorageBackendError, StorageBackendResult,
};

pub struct BuiltinRoutineCatalogInput {
    input: ImageDecoder<BuiltinRoutineSecurities, StorageBackendError>,
}

impl BuiltinRoutineCatalogInput {
    /// Reconstruct complete current ACLs under the selected physical transaction. The original read allowance remains with stored values; later mutations use the receiving registry's allowance.
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
                let records = rows(
                    catalog.as_ref(),
                    METADATA_PREFIX,
                    limits,
                    &retained,
                    &mut work,
                )
                .await?;
                let (records, _rows_memory) = records.into_parts();
                let mut registry = BuiltinRoutineSecurities::with_budget(retained.memory());
                for row in records {
                    work.checkpoint().await?;
                    production.check()?;
                    let oid = row
                        .name
                        .strip_prefix(METADATA_PREFIX)
                        .and_then(claim_oid)
                        .ok_or_else(|| {
                            StorageBackendError::Other("invalid builtin routine ACL key".into())
                        })?;
                    if crate::catalog::projection::builtin_routine_identity(oid).is_none() {
                        return Err(StorageBackendError::Other(format!(
                            "builtin routine ACL references missing function {oid}"
                        )));
                    }
                    let stored =
                        StoredSecurity::from_stored_json_with_control(&row.value, &production)
                            .map_err(decode_error)?
                            .into_budgeted()
                            .unwrap_or_else(|_| {
                                unreachable!("stored builtin ACL input is admitted")
                            });
                    if stored.builtin_routine_acl_format != 1 {
                        return Err(StorageBackendError::Other(format!(
                            "unknown builtin routine ACL format {}",
                            stored.builtin_routine_acl_format
                        )));
                    }
                    validate_from_input(&stored.entry, &roles, &production, &mut work)
                        .await
                        .map_err(sql_error)?;
                    if registry.contains_key(&oid) {
                        return Err(StorageBackendError::Other(
                            "duplicate builtin routine ACL record".into(),
                        ));
                    }
                    let (stored, memory) = stored.into_parts();
                    insert_stored_entry(&mut registry, oid, Budgeted::new(stored.entry, memory))
                        .map_err(sql_error)?;
                }
                production.check()?;
                Ok(registry.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }

    /// Failure, cancellation, zero work and unwind retire the entire unpublished generation.
    pub fn advance(
        &mut self,
        steps: usize,
    ) -> StorageBackendResult<Option<BuiltinRoutineSecurities>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

fn decode_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Internal(message) => {
            StorageBackendError::Serde(<serde_json::Error as serde::de::Error>::custom(message))
        }
        error => sql_error(error),
    }
}

fn sql_error(error: SQLError) -> StorageBackendError {
    match error {
        error @ SQLError::Internal(_) => StorageBackendError::Other(error.to_string()),
        SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("builtin routine ACL", error),
    }
}

#[cfg(test)]
mod tests;
