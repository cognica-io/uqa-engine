//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind view restoration to live registry guards and the catalog-opening mode.
use crate::{open::CatalogRestoreMode, CatalogFacade, Engine, StorageBackendResult, TableState};
use parking_lot::RwLockReadGuard;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_execution::catalog::view::{
    restoration::{
        self,
        context::{
            CatalogViewRows, ViewRestoreContext, ViewRestoreForeignRead, ViewRestoreGraphsRead,
            ViewRestoreNamespace, ViewRestoreSchemas, ViewRestoreSequences, ViewRestoreTableNames,
        },
    },
    StoredView, ViewRegistryRead, ViewRegistryState, ViewRegistryWrite,
};
use uqa_sql::{RowSchema, SQLError};

struct TableNamesRead<'a>(RwLockReadGuard<'a, BTreeMap<RelationIdentity, Arc<TableState>>>);
impl ViewRestoreTableNames for TableNamesRead<'_> {
    fn names(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_> {
        Box::new(self.0.keys())
    }
}
impl ViewRegistryState for Engine {
    fn views_read(&self) -> ViewRegistryRead<'_> {
        Box::new(self.durable.views.read())
    }
    fn views_write(&self) -> ViewRegistryWrite<'_> {
        Box::new(self.durable.views.write())
    }
}
impl ViewRestoreNamespace for Engine {
    fn tables(&self) -> Box<dyn ViewRestoreTableNames + '_> {
        Box::new(TableNamesRead(self.storage.tables.read()))
    }
    fn foreign_tables(&self) -> ViewRestoreForeignRead<'_> {
        Box::new(self.durable.foreign_tables.read())
    }
    fn graphs(&self) -> ViewRestoreGraphsRead<'_> {
        Box::new(self.durable.graphs.read())
    }
}
impl ViewRestoreSchemas for Engine {
    fn stored_schema(&self, view: &StoredView) -> Result<RowSchema, SQLError> {
        self.stored_view_schema(view)
    }
}
impl ViewRestoreSequences for Engine {
    fn resolve_loaded(&self, reference: &str) -> StorageBackendResult<String> {
        self.resolve_stored_sequence_reference_from_loaded_registry(reference)
    }
}
impl Engine {
    fn view_restore_context(&self) -> ViewRestoreContext<'_> {
        ViewRestoreContext {
            registry: self,
            namespace: self,
            roles: self,
            identities: self,
            bindings: self,
            schemas: self,
            sequences: self,
        }
    }
    pub(crate) fn restore_views_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        restoration::restore_views_from_catalog(
            &self.view_restore_context(),
            &CatalogViewRows { catalog },
            mode.allows_migration(),
        )
    }
}
