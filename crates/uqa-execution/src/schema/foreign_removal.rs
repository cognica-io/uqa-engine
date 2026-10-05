//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove a foreign table's own state and publish the removal inside the caller's transaction.
use crate::catalog::foreign::lookup::ForeignLookupContext;
use crate::schema::{
    events::context::EventLifecycleContext, foreign_table_alteration::ForeignTableAlterPublication,
    publication::dependencies::CatalogPublicationChanges,
};
use uqa_core::RelationIdentity;
use uqa_storage::CatalogFacade;
pub struct ForeignTableRemovalContext<'a> {
    pub lookup: ForeignLookupContext<'a>,
    pub publication: &'a dyn ForeignTableAlterPublication,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub events: EventLifecycleContext<'a>,
}
impl ForeignTableRemovalContext<'_> {
    pub fn drop_foreign_table_inner(&self, name: &str) -> Result<bool, String> {
        self.lookup
            .state
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let Some(name) = self
            .lookup
            .resolve_foreign_table_name(name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
        else {
            return Ok(false);
        };
        let relation = RelationIdentity::from_legacy_name(&name)?;
        if !self.lookup.registry.tables().contains_key(&relation) {
            return Err(format!("Foreign table `{name}` disappeared before drop"));
        }
        if !self.lookup.registry.security().contains_key(&relation) {
            return Err(format!(
                "Foreign table `{name}` has no loaded security metadata"
            ));
        }
        self.events
            .drop_relation_events_inner(&relation)
            .map_err(|error| format!("drop foreign table `{name}` events: {error}"))?;
        if let Some(catalog) = self.catalog {
            catalog
                .drop_foreign_table(&relation)
                .map_err(|err| format!("drop foreign table `{name}`: {err}"))?;
        }
        self.publication.memory_tables_write().remove(&relation);
        let mut tables = self.publication.tables_write();
        let mut table_security = self.publication.security_write();
        let removed = tables.remove(&relation).is_some();
        table_security.remove(&relation);
        drop(table_security);
        drop(tables);
        if removed {
            self.changes.catalog_registry_changed();
        }
        Ok(removed)
    }
}
