//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow pinned foreign definitions, live registry guards and session state for native catalog operations.
use crate::{Engine, RelationIdentity};
use std::collections::BTreeMap;
use uqa_execution::{
    catalog::foreign::{
        lookup::{ForeignLookupContext, ForeignLookupState},
        StoredForeignTable,
    },
    schema::foreign_removal::ForeignTableRemovalContext,
};
use uqa_storage::StorageBackendResult;
impl Engine {
    pub(crate) fn foreign_server_removal_publication(
        &self,
    ) -> uqa_execution::schema::foreign_server_removal::ForeignServerRemovalPublication<'_> {
        uqa_execution::schema::foreign_server_removal::ForeignServerRemovalPublication {
            registry: self,
            catalog: self.storage.catalog.as_deref(),
            changes: self,
        }
    }
    pub(crate) fn foreign_server_removal_context(
        &self,
    ) -> uqa_execution::schema::foreign_server_removal::ForeignServerRemovalContext<'_> {
        uqa_execution::schema::foreign_server_removal::ForeignServerRemovalContext {
            publication: self.foreign_server_removal_publication(),
            namespace: self,
            locks: self,
            writer: self,
            roles: self,
            session: self,
            deletion: self,
            notices: self.query_runtime_view().notices,
        }
    }
    pub(crate) fn foreign_lookup_context(&self) -> ForeignLookupContext<'_> {
        ForeignLookupContext {
            state: self,
            registry: self,
        }
    }
    pub(crate) fn foreign_removal_context(&self) -> ForeignTableRemovalContext<'_> {
        ForeignTableRemovalContext {
            lookup: self.foreign_lookup_context(),
            publication: self,
            catalog: self.storage.catalog.as_deref(),
            changes: self,
            events: self.event_lifecycle_context(),
        }
    }
}
impl ForeignLookupState for Engine {
    fn query_wrappers(&self) -> Option<&uqa_sql::catalog::foreign_wrapper::ForeignWrappers> {
        self.query_catalog_snapshot
            .as_ref()
            .map(|snapshot| snapshot.foreign_wrappers.as_ref())
    }
    fn query_servers(
        &self,
    ) -> Option<&BTreeMap<String, uqa_sql::catalog::foreign_server::ForeignServerDefinition>> {
        self.query_catalog_snapshot
            .as_ref()
            .map(|snapshot| snapshot.foreign_servers.as_ref())
    }
    fn query_tables(&self) -> Option<&BTreeMap<RelationIdentity, StoredForeignTable>> {
        self.query_catalog_snapshot
            .as_ref()
            .map(|snapshot| snapshot.foreign_tables.as_ref())
    }
    fn synchronize_catalog_registries(&self) -> StorageBackendResult<()> {
        Engine::synchronize_catalog_registries(self)
    }
    fn relation_lookup_candidates(
        &self,
        name: &str,
    ) -> StorageBackendResult<Vec<RelationIdentity>> {
        Engine::relation_lookup_candidates(self, name)
    }
}
