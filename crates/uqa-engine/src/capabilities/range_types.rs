//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain declared range generations and adapt the existing catalog transaction owner.

use crate::Engine;
use uqa_execution::catalog::range_type::{
    RangeRegistry, RangeRegistryPublication, RangeRegistryRead,
};

impl Engine {
    pub(crate) fn restore_ranges_from_catalog(
        &self,
        catalog: &dyn uqa_storage::CatalogFacade,
    ) -> uqa_storage::StorageBackendResult<()> {
        *self.durable.ranges.write() =
            uqa_execution::catalog::range_type::restore(catalog, &self.durable.roles.read())?;
        Ok(())
    }
}

impl RangeRegistryPublication for Engine {
    fn range_registry(&self) -> RangeRegistryRead<'_> {
        Box::new(self.durable.ranges.read())
    }

    fn range_catalog(&self) -> Option<&dyn uqa_storage::CatalogFacade> {
        self.storage.catalog.as_deref()
    }

    fn range_catalog_snapshot(&self) -> uqa_execution::catalog::CatalogReadView {
        self.restored_catalog_read_view()
    }

    fn range_role_definitions(
        &self,
    ) -> std::collections::BTreeMap<String, uqa_sql::catalog::roles::RoleDefinition> {
        self.durable.roles.read().clone()
    }

    fn publish_range_definitions(&self, registry: RangeRegistry) {
        *self.durable.ranges.write() = registry;
    }
}
