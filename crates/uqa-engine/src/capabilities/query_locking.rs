//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind tuple-lock execution to catalog, transaction, and storage state.

use crate::{capabilities::query_scope::CteScope, session::StatementReadSnapshot, Engine};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::DocId;
use uqa_execution::{
    query::locking::{
        context::{
            LockingCatalog, QueryRowLockSession, RowLockReadSource, RowLockScopeSource,
            RowRecheckBuilder,
        },
        RowLockContext,
    },
    row_locks::{retry_cache::RowLockRetryCache, RelationLockMode, RowLockAcquisition},
    PhysicalOperator,
};
use uqa_sql::{
    ast::{ColumnDef, RelationPersistence, TableKeyConstraint},
    plan::{QueryBlockPlan, QueryPlan},
    SQLError, SQLParam,
};
use uqa_storage::document_store::Document;

impl Engine {
    pub(crate) fn row_lock_context(&self) -> RowLockContext<'_, StatementReadSnapshot> {
        RowLockContext {
            catalog: self,
            session: self,
            rows: self,
            scopes: self,
            recheck: self,
            cancellation: self.query_runtime_view().cancellation,
        }
    }
}
impl LockingCatalog for Engine {
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        Engine::hierarchy_scan_tables(self, table, descendants)
    }
    fn resolve_relation(
        &self,
        name: &str,
        bound: bool,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        self.try_resolve_relation_kind_for_query(name, bound)
    }
    fn view_plan(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
        Engine::view_plan(self, name)
    }
    fn table_persistence(&self, name: &str) -> Result<Option<RelationPersistence>, String> {
        Engine::table_persistence(self, name).map_err(|error| error.to_string())
    }
    fn referenceable_keys(&self, table: &str) -> Result<Vec<TableKeyConstraint>, String> {
        Engine::referenceable_keys(self, table).map_err(|error| error.to_string())
    }
    fn table_columns(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        self.try_describe_table(table)
            .map_err(|error| error.to_string())
    }
}
impl QueryRowLockSession for Engine {
    fn lock_relation(&self, table: &str, mode: RelationLockMode) -> Result<(), SQLError> {
        Engine::lock_relation(self, table, mode)
    }
    fn rollback_row_lock_acquisition(&self, acquisition: RowLockAcquisition) {
        Engine::rollback_row_lock_acquisition(self, acquisition);
    }
    fn row_changed_in_open_transaction(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<bool, SQLError> {
        Engine::row_changed_in_open_transaction(self, table, doc_id)
    }
    fn row_lock_change_requires_recheck(&self) -> Result<bool, SQLError> {
        Engine::row_lock_change_requires_recheck(self)
    }
    fn current_transaction_is_read_only(&self) -> bool {
        Engine::current_transaction_is_read_only(self)
    }
    fn statement_row_lock_cache(&self) -> Result<Arc<RowLockRetryCache>, SQLError> {
        Engine::statement_row_lock_cache(self)
    }
}
impl RowLockReadSource for Engine {
    fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError> {
        Engine::get_document(self, table, doc_id)
    }
    fn get_document_for_mutation(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<Document>, SQLError> {
        Engine::get_document_for_mutation(self, table, doc_id)
    }
}
impl RowLockScopeSource<StatementReadSnapshot> for Engine {
    fn current_routine_scope(&self) -> CteScope {
        crate::capabilities::query_scope::new_for_current_routine(self)
    }
    fn transition_relation_names(&self) -> BTreeSet<String> {
        uqa_execution::mutation::triggers::current_transition_relation_names()
    }
}
impl RowRecheckBuilder<StatementReadSnapshot> for Engine {
    fn build_recheck<'a>(
        &'a self,
        statement: &QueryBlockPlan,
        params: &'a [SQLParam],
        ctes: &mut CteScope,
        ordered: bool,
        projections: &[uqa_execution::query::PhysicalProjection],
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        uqa_execution::query::sources::recheck::build_row_lock_recheck_operator(
            &self.source_execution_context(),
            statement,
            params,
            ctes,
            ordered,
            projections,
        )
    }
}
