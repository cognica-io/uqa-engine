//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current database authority retains generic marker selection, native fields and original role order.

use super::{DatabaseAuthority, DATABASE_SECURITY_METADATA_KEY};
use crate::catalog::type_records::marker;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget, ProductionControl},
};
use uqa_sql::{
    catalog::{
        roles::RoleDefinitions,
        security::{
            database::stored::{adopt, StoredSecurity},
            projected_input::ScalarProtocol,
        },
    },
    SQLError,
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
    StorageBackendError, StorageBackendResult,
};

pub struct DatabaseCatalogInput {
    input: ImageDecoder<DatabaseAuthority, StorageBackendError>,
}
impl DatabaseCatalogInput {
    /// Reconstruct current authority inside the caller's selected catalog transaction. Initial legacy migration and ordinary refresh retain their existing owners.
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
                work.checkpoint().await?;
                let json = catalog.get_metadata_with_control(
                    DATABASE_SECURITY_METADATA_KEY,
                    limits,
                    &retained,
                )?;
                let json = json.ok_or_else(migration)?;
                let marker =
                    marker::field_from(&json, "database_security_format", &production, &mut work)
                        .await?;
                if !marker.present {
                    return Err(migration());
                }
                work.checkpoint().await?;
                let keys = marker::protocol::keys(&production)?;
                let stored = StoredSecurity::from_stored_value_json(
                    &json,
                    ScalarProtocol {
                        number: keys.number,
                        raw: keys.raw,
                    },
                    &production,
                )
                .map_err(decode_error)?
                .into_budgeted()
                .unwrap_or_else(|_| unreachable!("database authority input is admitted"));
                if stored.database_security_format != 1 {
                    return Err(StorageBackendError::Other(format!(
                        "unsupported database security format {}",
                        stored.database_security_format
                    )));
                }
                stored
                    .security
                    .validate_from_input(&roles, &production, &mut work)
                    .await
                    .map_err(sql_error)?;
                let (stored, memory) = stored.into_parts();
                let value = adopt(
                    Budgeted::new(stored.security, memory),
                    &mutations,
                    retained.cancellation(),
                )
                .map_err(sql_error)?;
                production.check()?;
                Ok(value)
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<DatabaseAuthority>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}
fn migration() -> StorageBackendError {
    StorageBackendError::Other("database security requires initial catalog migration".into())
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
        SQLError::Internal(message) => StorageBackendError::Other(message),
        SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("database authority restoration", error),
    }
}

#[cfg(test)]
mod tests;
