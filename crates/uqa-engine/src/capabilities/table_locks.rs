//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind explicit relation locking to live catalogs and transaction-owned lock marks.

use crate::Engine;
use uqa_execution::{
    row_locks::{RelationLockMode, ScopedRelationLock},
    statement::table_locks::{
        TableLockCatalog, TableLockContext, TableLockMetadata, TableLockSession,
    },
};
use uqa_sql::{
    catalog::{resolution::RelationResolution, stored_view::StoredView},
    SQLError,
};

impl Engine {
    pub(crate) fn table_lock_context(&self) -> TableLockContext<'_> {
        TableLockContext {
            catalog: self,
            roles: self,
            session: self,
        }
    }
}

impl TableLockCatalog for Engine {
    fn resolve(&self, name: &str, bound: bool) -> Result<RelationResolution, SQLError> {
        if bound {
            self.resolve_bound_relation_kind(name)
        } else {
            self.resolve_visible_relation_kind(name)
        }
    }

    fn table(&self, name: &str) -> Result<Option<TableLockMetadata>, SQLError> {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        Ok(self
            .storage
            .tables
            .read()
            .get(&relation)
            .map(|table| TableLockMetadata {
                object_id: table.object_id(),
                security: table.security(),
            }))
    }

    fn view(&self, name: &str) -> Result<Option<StoredView>, SQLError> {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        Ok(self.durable.views.read().get(&relation).cloned())
    }

    fn table_name(&self, object_id: [u8; 16]) -> Option<String> {
        self.storage.tables.read().iter().find_map(|(name, table)| {
            (table.object_id() == object_id).then(|| name.qualified_name())
        })
    }

    fn descendants(&self, name: &str) -> Result<Vec<String>, SQLError> {
        self.hierarchy_scan_tables(name, true)
    }
}

impl TableLockSession for Engine {
    fn in_transaction_block(&self) -> bool {
        Engine::in_transaction_block(self)
    }
    fn current_user(&self) -> String {
        self.current_user_name()
    }
    fn acquire(
        &self,
        name: &str,
        mode: RelationLockMode,
        nowait: bool,
    ) -> Result<Option<ScopedRelationLock<'_>>, SQLError> {
        self.prepare_transaction_lock_wait()?;
        let table = self.row_locks.table_key(name);
        let marks = self.temporary_relation_lock_marks()?;
        if nowait {
            self.row_locks.try_acquire_scoped_relation(
                self.session_id,
                table,
                mode,
                marks,
                &self.runtime.cancellation,
            )
        } else {
            self.row_locks
                .acquire_scoped_relation(
                    self.session_id,
                    table,
                    mode,
                    marks,
                    &self.runtime.cancellation,
                )
                .map(Some)
        }
    }
    fn refresh_after_wait(&self) -> Result<(), SQLError> {
        self.refresh_explicit_statement_snapshot()
    }
}
