//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Services for row validation, key reservations, and referenced-row checks.
use crate::row_locks::{session::RowLockSession, LockAcquire};
use uqa_core::{DocId, PostingList, Predicate, Value};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    ast::ForeignKey,
    semantics::{partition::PartitionContext, referential::ReferentialCatalog},
    SQLError,
};
use uqa_storage::{document_store::Document, ValueIndexKey};

pub use uqa_sql::semantics::constraint_catalog::ConstraintCatalog;
/// Mutation-visible row images and the identities changed by an active command overlay.
pub trait MutationRead {
    fn table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError>;
    fn live_table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError>;
    fn live_table_doc_id_page(
        &self,
        table: &str,
        after: Option<DocId>,
        limit: usize,
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError>;
    fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError>;
    /// Read the stored fields without materializing expressions from the current descriptor; a retained rewrite supplies its original descriptor.
    fn raw_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError>;
    /// The changes of `table` that the active command overlays and this transaction's fixed-snapshot reads hold above its storage view, without copying them.
    fn command_overlay_changes(
        &self,
        table: &str,
    ) -> Result<Option<crate::query::document_changes::DocumentChanges>, SQLError>;
}
pub trait MutationIndexRead {
    fn index_definitions(
        &self,
    ) -> Result<std::sync::Arc<crate::catalog::index::physical::PhysicalIndexDefinitions>, SQLError>;
    fn find_conflict(
        &self,
        table: &str,
        columns: &[String],
        values: &[Value],
    ) -> Result<Option<DocId>, SQLError>;
    /// The visible rows the active commands staged for `table` whose `columns` hold `values`, a missing column holding null.
    fn staged_matches(
        &self,
        table: &str,
        columns: &[String],
        values: &[Value],
    ) -> Result<Vec<DocId>, SQLError>;
    /// Probe already evaluated physical tuple keys and retain only the command rows that mask the mutation-visible stored index.
    fn staged_expression_matches(
        &self,
        table: &str,
        physical_key: &str,
        values: &[Value],
    ) -> Result<crate::mutation::overlay::CommandIndexProbe, SQLError>;
    fn value_index_scan_key(
        &self,
        table: &str,
        key: &ValueIndexKey,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError>;
}
pub trait ConstraintTransactions {
    fn foreign_key_is_deferred(&self, table: &str, key: &ForeignKey) -> Result<bool, SQLError>;
    /// Whether the checks that a change to a referenced row fires are deferred, under the constraint `derived` names when the foreign key derives one on the firing partition.
    fn referenced_key_is_deferred(
        &self,
        table: &str,
        key: &ForeignKey,
        derived: Option<&uqa_sql::ast::ReferencedPartitionConstraint>,
    ) -> Result<bool, SQLError>;
    fn refresh_explicit_statement_snapshot(&self) -> Result<(), SQLError>;
    fn lock_key_reservation(&self, key: [u8; 32], table: &str) -> Result<LockAcquire, SQLError>;
    /// All keys are evaluated before this boundary, with no reads or callbacks between reservations.
    fn lock_key_reservations(
        &self,
        keys: &[[u8; 32]],
        table: &str,
    ) -> Result<Vec<LockAcquire>, SQLError> {
        keys.iter()
            .map(|key| self.lock_key_reservation(*key, table))
            .collect()
    }
}
pub trait MutationNamespace {
    fn current_role(&self) -> RoleReference;
    fn require_schema_privilege(
        &self,
        schema: &str,
        role: &RoleReference,
        privilege: crate::catalog::security::schema::SchemaAclPrivilege,
    ) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct ConstraintContext<'a> {
    pub catalog: &'a dyn ConstraintCatalog,
    pub values: &'a (dyn uqa_sql::expr::SQLValueCatalog + Send + Sync),
    pub reads: &'a dyn MutationRead,
    pub indexes: &'a dyn MutationIndexRead,
    pub transactions: &'a dyn ConstraintTransactions,
    pub locks: &'a dyn RowLockSession,
    pub namespace: &'a dyn MutationNamespace,
    pub referrers: &'a dyn ReferentialCatalog,
    pub partitions: PartitionContext<'a>,
    pub diagnostics: &'a dyn ConstraintDiagnosticSource,
    /// The memory a constraint's validation may sort keys in before it spills.
    pub memory: &'a dyn crate::query::runtime::QueryMemorySettings,
}

/// Capture catalog output and authority only after a key conflict has been found.
pub trait ConstraintDiagnosticSource {
    fn diagnostic_context(&self) -> ConstraintDiagnosticContext<'_>;
}

#[derive(Clone, Copy)]
pub struct ConstraintDiagnosticContext<'a> {
    pub catalog: crate::catalog::context::CatalogContext<'a>,
    pub authorization: crate::catalog::security::table_authorization::TableAuthorizationContext<'a>,
}
