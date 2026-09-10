//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable row writes and transaction publication state.
use crate::mutation::{constraints::context::ConstraintCatalog, identity::MutationIdentifiers};
use std::collections::BTreeMap;
use uqa_core::{DocId, FieldName};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;
pub type DocumentVectors = BTreeMap<FieldName, Vec<Vec<f32>>>;
pub trait MutationStorage {
    fn delete_document(&self, table: &str, doc_id: DocId) -> Result<(), SQLError>;
    fn insert_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: DocumentVectors,
        known_new: bool,
    ) -> Result<(), SQLError>;
    fn insert_document_deferred_text(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: DocumentVectors,
        known_new: bool,
    ) -> Result<(), SQLError>;
    fn rewrite_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError>;
}
pub trait MutationTextIndex {
    fn text_fields(
        &self,
        table: &str,
        document: &Document,
    ) -> Result<BTreeMap<FieldName, String>, SQLError>;
    fn add_documents(
        &self,
        table: &str,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> Result<(), SQLError>;
}
pub trait MutationHistory {
    fn note_rewrite(
        &self,
        old_table: &str,
        old_doc_id: DocId,
        new_table: &str,
        new_doc_id: DocId,
    ) -> Result<(), SQLError>;
}
pub trait MutationConstraintDeferrals {
    fn inserted(&self, table: &str, doc_id: DocId) -> Result<(), SQLError>;
    fn rewritten(
        &self,
        table: &str,
        doc_id: DocId,
        old: Option<&Document>,
        new: &Document,
    ) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct PublicationContext<'a> {
    pub storage: &'a dyn MutationStorage,
    pub text: &'a dyn MutationTextIndex,
    pub history: &'a dyn MutationHistory,
    pub deferrals: &'a dyn MutationConstraintDeferrals,
    pub identifiers: &'a dyn MutationIdentifiers,
    pub catalog: &'a dyn ConstraintCatalog,
}
