//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind composite type DDL to namespace, identity and publication state, and expose composite attributes to value conversion.

use crate::Engine;
use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_execution::catalog::composite_type::{
    CompositeRegistry, CompositeRegistryPublication, CompositeRegistryRead,
};
use uqa_execution::schema::composites::attributes::CompositeAttributeContext;
use uqa_execution::schema::composites::values::{CompositeValueContext, CompositeValueTables};
use uqa_execution::schema::composites::CompositeTypeContext;
use uqa_sql::catalog::roles::RoleDefinition;
use uqa_sql::expr::composites::{CompositeTypeCatalog, CompositeTypeDescriptor};
use uqa_sql::SQLError;

impl Engine {
    pub(crate) fn composite_type_context(&self) -> CompositeTypeContext<'_> {
        CompositeTypeContext {
            creation: self.relation_creation_context(),
            identities: self.catalog_identity_reservation_context(),
            writer: self,
            types: self,
            allocate_identity: || {
                crate::new_nonzero_catalog_identity("composite", "object identity")
                    .map_err(|error| SQLError::Internal(error.to_string()))
            },
            publication: self,
            enums: self,
            domains: self,
            changes: self,
        }
    }

    pub(crate) fn composite_value_context(&self) -> CompositeValueContext<'_> {
        CompositeValueContext {
            memory: self.session.as_ref(),
            cancellation: &self.runtime.cancellation,
            tables: self,
            reads: self,
            writes: self,
            views: self.view_dependency_context(),
            types: self,
            catalogs:
                uqa_execution::schema::composites::catalog_values::CompositeCatalogValueContext {
                    schema: self.schema_dependency_publication_context(),
                    domains: self,
                    events: self.event_catalog_context(),
                    routines: self.routine_mutation_context(),
                    indexes: uqa_execution::schema::indexes::routines::IndexRoutineContext {
                        registry: self,
                        catalog: self.storage.catalog.as_deref(),
                    },
                    index_publication: self,
                    types: self,
                    bindings: self,
                    resolution: self,
                },
        }
    }

    pub(crate) fn composite_attribute_context(&self) -> CompositeAttributeContext<'_> {
        CompositeAttributeContext {
            publication: self,
            values: self.composite_value_context(),
            changes: self,
        }
    }

    /// Composite definitions restore after enums and before domains and relations, whose declared types can refer to them.
    pub(crate) fn restore_composites_from_catalog(
        &self,
        catalog: &dyn uqa_storage::CatalogFacade,
    ) -> uqa_storage::StorageBackendResult<()> {
        let registry =
            uqa_execution::catalog::composite_type::restore(catalog, &self.durable.roles.read())?;
        *self.durable.composites.write() = registry;
        Ok(())
    }

    /// The composite registry generation of the running statement: the pinned query snapshot when one exists, otherwise the session's current definitions.
    pub(crate) fn composite_registry_snapshot(&self) -> Arc<CompositeRegistry> {
        self.query_catalog_snapshot.as_ref().map_or_else(
            || self.durable.composites.snapshot(),
            |snapshot| Arc::clone(&snapshot.composites),
        )
    }
}

impl CompositeTypeCatalog for Engine {
    fn composite_type(
        &self,
        type_oid: u32,
    ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        let descriptor = self
            .runtime
            .composite_descriptor_cache
            .descriptor(&self.composite_registry_snapshot(), type_oid);
        match descriptor {
            Some(descriptor) => Ok(Some(descriptor)),
            None => uqa_execution::catalog::composite_type::relations::descriptor(
                &self.catalog_execution(),
                type_oid,
            ),
        }
    }
}

impl CompositeValueTables for Engine {
    fn composite_value_tables(
        &self,
    ) -> Result<Vec<(String, Vec<uqa_sql::ast::ColumnDef>)>, SQLError> {
        let catalog = self.catalog_read_view();
        Ok(catalog
            .snapshot()
            .tables
            .iter()
            .map(|(identity, table)| (identity.qualified_name(), table.columns.as_ref().clone()))
            .collect())
    }
}

impl CompositeRegistryPublication for Engine {
    fn composite_registry(&self) -> CompositeRegistryRead<'_> {
        Box::new(self.durable.composites.read())
    }
    fn composite_catalog(&self) -> Option<&dyn uqa_storage::CatalogFacade> {
        self.storage.catalog.as_deref()
    }
    fn composite_role_definitions(&self) -> BTreeMap<String, RoleDefinition> {
        self.durable.roles.read().clone()
    }
    fn publish_composite_definitions(&self, registry: CompositeRegistry) {
        *self.durable.composites.write() = registry;
    }
}
