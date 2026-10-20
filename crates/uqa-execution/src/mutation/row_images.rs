//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed old and new row images shared by mutation RETURNING and event paths.

use crate::OwnedPhysicalRow;
use uqa_core::DocId;
use uqa_storage::{document_store::Document, DocumentMetadata};

#[derive(Clone)]
pub struct RuleRowImage {
    pub old_storage_table: Option<String>,
    pub old_doc_id: Option<DocId>,
    pub old: Option<Document>,
    pub new_storage_table: Option<String>,
    pub new_doc_id: Option<DocId>,
    pub new: Option<Document>,
    pub context: Option<OwnedPhysicalRow>,
}

impl RuleRowImage {
    pub fn empty() -> Self {
        Self {
            old_storage_table: None,
            old_doc_id: None,
            old: None,
            new_storage_table: None,
            new_doc_id: None,
            new: None,
            context: None,
        }
    }

    pub fn supplement_documents(&mut self, supplemental: Self) {
        supplement_document(&mut self.old, supplemental.old);
        supplement_document(&mut self.new, supplemental.new);
    }

    pub(crate) fn from_delete_candidate(
        candidate: super::candidate::MutationCandidate<Option<OwnedPhysicalRow>>,
    ) -> Self {
        Self {
            old_storage_table: Some(candidate.identity.table),
            old_doc_id: Some(candidate.identity.doc_id),
            old: Some(candidate.document),
            context: candidate.context,
            ..Self::empty()
        }
    }

    pub(crate) fn into_delete_candidate(
        self,
    ) -> Result<super::candidate::MutationCandidate<Option<OwnedPhysicalRow>>, uqa_sql::SQLError>
    {
        let invalid = || {
            uqa_sql::SQLError::Internal(
                "rewrite-rule event lost its original delete candidate".into(),
            )
        };
        if self.new_storage_table.is_some() || self.new_doc_id.is_some() || self.new.is_some() {
            return Err(invalid());
        }
        Ok(super::candidate::MutationCandidate {
            identity: super::candidate::PhysicalDocumentIdentity {
                table: self.old_storage_table.ok_or_else(invalid)?,
                doc_id: self.old_doc_id.ok_or_else(invalid)?,
            },
            document: self.old.ok_or_else(invalid)?,
            context: self.context,
        })
    }

    pub(crate) fn from_rewrite_candidate(
        candidate: super::candidate::MutationRewriteCandidate<Option<OwnedPhysicalRow>>,
    ) -> Self {
        Self {
            old_storage_table: Some(candidate.identity.table.clone()),
            old_doc_id: Some(candidate.identity.doc_id),
            old: Some(candidate.old_document),
            new_storage_table: Some(candidate.identity.table),
            new_doc_id: Some(candidate.identity.doc_id),
            new: Some(candidate.proposed_document),
            context: candidate.context,
        }
    }

    pub(crate) fn into_rewrite_candidate(
        self,
    ) -> Result<
        super::candidate::MutationRewriteCandidate<Option<OwnedPhysicalRow>>,
        uqa_sql::SQLError,
    > {
        let invalid = || {
            uqa_sql::SQLError::Internal(
                "rewrite-rule event lost its original update candidate".into(),
            )
        };
        if self.old_storage_table != self.new_storage_table || self.old_doc_id != self.new_doc_id {
            return Err(invalid());
        }
        Ok(super::candidate::MutationRewriteCandidate {
            identity: super::candidate::PhysicalDocumentIdentity {
                table: self.old_storage_table.ok_or_else(invalid)?,
                doc_id: self.old_doc_id.ok_or_else(invalid)?,
            },
            old_document: self.old.ok_or_else(invalid)?,
            proposed_document: self.new.ok_or_else(invalid)?,
            context: self.context,
        })
    }
}

fn supplement_document(target: &mut Option<Document>, supplemental: Option<Document>) {
    let Some(supplemental) = supplemental else {
        return;
    };
    if let Some(target) = target {
        target.extend(supplemental);
    } else {
        *target = Some(supplemental);
    }
}

#[derive(Clone)]
pub struct MutationRowImage<'a> {
    pub storage_table: String,
    pub doc_id: DocId,
    pub document: &'a Document,
    pub metadata: DocumentMetadata,
}

#[derive(Clone)]
pub struct MutationRowImages<'a> {
    pub old: Option<MutationRowImage<'a>>,
    pub new: Option<MutationRowImage<'a>>,
}

impl uqa_sql::semantics::rules::binding::RuleRowValues for RuleRowImage {
    fn old_row(&self) -> Option<&uqa_sql::ResultRow> {
        self.old.as_ref()
    }
    fn new_row(&self) -> Option<&uqa_sql::ResultRow> {
        self.new.as_ref()
    }
    fn old_doc_id(&self) -> Option<DocId> {
        self.old_doc_id
    }
    fn new_doc_id(&self) -> Option<DocId> {
        self.new_doc_id
    }
}
