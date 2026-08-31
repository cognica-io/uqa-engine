//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared mutation identities, prepared actions, locking, event state, and command scopes.

mod candidate;
mod command;
mod deferred;
mod events;
mod locking;
mod prepared;
mod publication;
mod row_images;

pub(in crate::sql) use candidate::{
    MutationCandidate, MutationLockTarget, MutationRewriteCandidate, PhysicalDocumentIdentity,
    PhysicalMutationLockTarget,
};
pub(in crate::sql) use command::{run_mutation_command, MutationOverlayScope};
pub(crate) use command::{CommandExactIndex, CommandMutationOverlay};
pub(crate) use deferred::DeferredForeignKeyCheck;
pub(in crate::sql) use events::{
    MutationEventQueue, ReferentialActionContext, ReferentialRewritePreparation,
};
pub(in crate::sql) use locking::{
    concurrent_update_serialization_failure, lock_mutation_row, lock_mutation_target,
    lock_physical_mutation_target, MutationLockCleanup,
};
pub(in crate::sql) use prepared::{
    decode_prepared_doc_id, decode_prepared_insert_conflict, decode_prepared_mutation_action,
    encode_prepared_doc_id, encode_prepared_insert_conflict, encode_prepared_mutation_action,
    PreparedDeleteAction, PreparedDocumentDelete, PreparedDocumentInsert, PreparedDocumentRewrite,
    PreparedInsertConflict, PreparedMutationAction,
};
pub(crate) use publication::TransactionRowChange;
pub(in crate::sql) use publication::{
    finish_mutation_publication, publish_prepared_mutation_action, MutationPublicationBatch,
};
pub(in crate::sql) use row_images::{MutationRowImage, MutationRowImages, RuleRowImage};
