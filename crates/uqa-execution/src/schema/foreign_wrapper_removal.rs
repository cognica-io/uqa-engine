//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish wrapper removal after the dependency plan removes its servers and foreign tables.

use super::{
    foreign_creation::ForeignCreationRegistry, publication::dependencies::CatalogPublicationChanges,
};
use uqa_sql::SQLError;
use uqa_storage::CatalogFacade;

pub struct ForeignWrapperRemovalPublication<'a> {
    pub registry: &'a dyn ForeignCreationRegistry,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
}

impl ForeignWrapperRemovalPublication<'_> {
    /// The caller retains the object lock and has already checked the deletion's authority.
    pub fn remove(&self, name: &str, object_id: [u8; 16]) -> Result<(), SQLError> {
        if self
            .registry
            .wrappers()
            .get(name)
            .is_none_or(|wrapper| wrapper.identity.object_id != object_id)
        {
            return Err(SQLError::Internal(format!(
                "foreign-data wrapper `{name}` changed before catalog removal"
            )));
        }
        if let Some(catalog) = self.catalog {
            crate::catalog::foreign::wrappers::remove(catalog, name).map_err(|error| {
                uqa_sql::catalog::errors::storage_error("DROP FOREIGN DATA WRAPPER", &error)
            })?;
        }
        self.registry.wrappers_write().remove(name);
        self.changes.catalog_registry_changed();
        Ok(())
    }
}
