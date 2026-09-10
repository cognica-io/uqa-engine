//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Services for row validation, key reservations, and referenced-row checks.
use crate::row_locks::{session::RowLockSession, LockAcquire};
use std::collections::BTreeSet;
use uqa_core::{DocId, PostingList, Predicate, Value};
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
    fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError>;
    fn command_overlay_changed_ids(&self, table: &str)
        -> Result<Option<BTreeSet<DocId>>, SQLError>;
}
pub trait MutationIndexRead {
    fn find_conflict(
        &self,
        table: &str,
        columns: &[String],
        values: &[Value],
    ) -> Result<Option<DocId>, SQLError>;
    fn value_index_scan_key(
        &self,
        table: &str,
        key: &ValueIndexKey,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError>;
}
pub trait ConstraintTransactions {
    fn foreign_key_is_deferred(&self, table: &str, key: &ForeignKey) -> Result<bool, SQLError>;
    fn refresh_explicit_statement_snapshot(&self) -> Result<(), SQLError>;
    fn lock_key_reservation(&self, key: [u8; 32], table: &str) -> Result<LockAcquire, SQLError>;
}
pub trait MutationNamespace {
    fn current_user_name(&self) -> String;
    fn require_schema_privilege(
        &self,
        schema: &str,
        role: &str,
        privilege: crate::catalog::security::schema::SchemaAclPrivilege,
    ) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct ConstraintContext<'a> {
    pub catalog: &'a dyn ConstraintCatalog,
    pub reads: &'a dyn MutationRead,
    pub indexes: &'a dyn MutationIndexRead,
    pub transactions: &'a dyn ConstraintTransactions,
    pub locks: &'a dyn RowLockSession,
    pub namespace: &'a dyn MutationNamespace,
    pub referrers: &'a dyn ReferentialCatalog,
    pub partitions: PartitionContext<'a>,
}
