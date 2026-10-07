//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind the catalog services that remove each kind of object to the live registries, session locks and notices.

use crate::Engine;
use uqa_execution::schema::deletion::{CatalogRemovalContext, CatalogRemovalInputs};

impl CatalogRemovalInputs for Engine {
    fn catalog_removal_context(&self) -> CatalogRemovalContext<'_> {
        CatalogRemovalContext {
            catalog: self.catalog_execution(),
            locks: self,
            shared_locks: self,
            identities: self,
            tables: self.table_removal_context(),
            foreign_tables: self.foreign_removal_context(),
            foreign_servers: self.foreign_server_removal_publication(),
            foreign_wrappers:
                uqa_execution::schema::foreign_wrapper_removal::ForeignWrapperRemovalPublication {
                    registry: self,
                    catalog: self.storage.catalog.as_deref(),
                    changes: self,
                },
            indexes: self.index_removal_context(),
            domains: self.domain_dependency_context(),
            composites: self.composite_attribute_context(),
            schemas: self.empty_schema_removal_context(),
            events: self,
            notices: self.query_runtime_view().notices,
        }
    }
}
