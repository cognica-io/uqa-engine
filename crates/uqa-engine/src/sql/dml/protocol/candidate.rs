//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed physical identities and committed-chain lock outcomes shared by every DML command.

use uqa_core::DocId;
use uqa_storage::document_store::Document;

#[derive(Debug, Clone, PartialEq)]
pub(in crate::sql) struct MutationCandidate<C = ()> {
    pub identity: PhysicalDocumentIdentity,
    pub document: Document,
    pub context: C,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::sql) struct MutationRewriteCandidate<C = ()> {
    pub identity: PhysicalDocumentIdentity,
    pub old_document: Document,
    pub proposed_document: Document,
    pub context: C,
}

/// Outcome of following a logical DML target through its committed update chain.
pub(in crate::sql) enum MutationLockTarget {
    Present { doc_id: DocId, recheck: bool },
    Deleted,
}

/// Outcome of following a physical DML target across primary-key rewrites and partition moves.
pub(in crate::sql) enum PhysicalMutationLockTarget {
    Present {
        identity: PhysicalDocumentIdentity,
        recheck: bool,
    },
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::sql) struct PhysicalDocumentIdentity {
    pub table: String,
    pub doc_id: DocId,
}
