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
    pub(crate) fn range_input_context<'a>(
        &'a self,
        catalog: &'a uqa_execution::catalog::CatalogReadView,
        expressions: &'a dyn uqa_sql::expr::EngineHook,
    ) -> uqa_execution::catalog::range_type::input::RangeInputContext<'a> {
        uqa_execution::catalog::range_type::input::RangeInputContext {
            catalog,
            expressions,
            canonical: self,
            states: Some(&self.session.range_comparison_states),
        }
    }

    pub(crate) fn restore_ranges_from_catalog(
        &self,
        catalog: &dyn uqa_storage::CatalogFacade,
    ) -> uqa_storage::StorageBackendResult<()> {
        *self.durable.ranges.write() =
            uqa_execution::catalog::range_type::restore(catalog, &self.durable.roles.read())?;
        Ok(())
    }
}

impl uqa_execution::catalog::range_type::input::RangeCanonicalInvocation for Engine {
    fn invoke_canonical(
        &self,
        catalog: &uqa_execution::catalog::CatalogReadView,
        oid: u32,
        value: &uqa_core::Value,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<uqa_core::Value>, uqa_sql::SQLError> {
        uqa_execution::routines::invocation::call_range_canonical(
            &self.routine_invocation_context(),
            catalog,
            oid,
            value,
            control,
        )
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
