//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine restoration retains its complete selected catalog and admitted session namespace.

use super::{
    RoutineRestoreSchemaInput, RoutineTypeIdentityInput, SchemaRegistry, StorageBackendError,
    StorageBackendResult,
};
use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl},
};
use uqa_sql::{ast::UserTypeIdentity, SQLError};

pub struct RoutineCatalogTypeNames {
    catalog: CatalogReadView,
    resolution: Produced<RelationNameResolution>,
}
impl RoutineCatalogTypeNames {
    /// Move already captured catalog and session inputs into restoration. Capturing Engine table/session owners remains at their existing adapter boundary.
    pub fn new(catalog: CatalogReadView, resolution: Produced<RelationNameResolution>) -> Self {
        Self {
            catalog,
            resolution,
        }
    }
}
impl RoutineRestoreSchemaInput for RoutineCatalogTypeNames {
    fn schemas(&self) -> &SchemaRegistry {
        &self.catalog.snapshot().definitions.schemas
    }
}
impl RoutineTypeIdentityInput for RoutineCatalogTypeNames {
    type Error = StorageBackendError;
    async fn resolve_user_type_identity(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<UserTypeIdentity>> {
        crate::catalog::projection::routine_type_name_identity_from_input(
            &self.catalog,
            &self.resolution,
            name,
            control,
            input,
        )
        .await
        .map_err(|error| match error {
            SQLError::Cancelled(error) => error.into(),
            error => StorageBackendError::backend("routine type identity", error),
        })
    }
}
