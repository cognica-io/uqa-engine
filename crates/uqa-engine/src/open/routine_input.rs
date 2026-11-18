//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select the private session's catalog and namespace for stored routine signatures.

use super::Engine;
use crate::{capabilities::RestoreCatalogSelection, state::SessionNamespace};
use std::sync::Arc;
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl, CancellationToken};
use uqa_execution::routines::catalog::input::{
    RoutineCatalogPlaceholderInput, RoutineCatalogTypeNames, RoutineRestoreSchemaInput,
};
use uqa_sql::{
    ast::UserTypeIdentity, catalog::security::schema_registry::SchemaRegistry,
    routines::lifecycle::restoration::RoutineTypeIdentityInput, SQLError,
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};

struct SelectedCatalog {
    catalog: RestoreCatalogSelection,
    namespace: SessionNamespace,
}

struct RoutineNames {
    schemas: SchemaRegistry,
    selected: Option<SelectedCatalog>,
    names: Option<RoutineCatalogTypeNames>,
    original: CancellationToken,
}

impl RoutineRestoreSchemaInput for RoutineNames {
    fn schemas(&self) -> &SchemaRegistry {
        &self.schemas
    }
}

impl RoutineTypeIdentityInput for RoutineNames {
    type Error = StorageBackendError;

    async fn resolve_user_type_identity(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<UserTypeIdentity>> {
        // Metadata and declaration errors precede the first lookup. Empty catalogs and signatures
        // that already record every type by identity never construct a table or search-path view.
        if let Some(selected) = self.selected.take() {
            let catalog = selected
                .catalog
                .read(
                    control.budget().expect("native routine catalog"),
                    &self.original,
                    input,
                )
                .await
                .map_err(catalog_error)?;
            let namespace = selected
                .namespace
                .capture_from_input(control, input)
                .await
                .map_err(catalog_error)?;
            self.names = Some(RoutineCatalogTypeNames::new(catalog, namespace));
        }
        self.names
            .as_mut()
            .expect("selected catalog captured on its first type lookup")
            .resolve_user_type_identity(name, control, input)
            .await
    }
}

fn catalog_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("routine type catalog", error),
    }
}

impl Engine {
    pub(super) fn routine_placeholder_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<RoutineCatalogPlaceholderInput> {
        let durable = self.durable.snapshot();
        let names = RoutineNames {
            schemas: (*durable.schemas).clone(),
            selected: Some(SelectedCatalog {
                catalog: self.select_restore_catalog(&durable),
                namespace: self.session.namespace_snapshot(self.session_id),
            }),
            names: None,
            original: control.cancellation().clone(),
        };
        let previous = &*durable.sql_user_functions;
        RoutineCatalogPlaceholderInput::new(
            Arc::clone(self.storage.catalog.as_ref().expect("persistent catalog")),
            (*durable.roles).clone(),
            names,
            previous.clone(),
            limits,
            control.memory(),
            control,
        )
    }
}
