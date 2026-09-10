//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind physical publication to the active storage and transaction generation.
use crate::Engine;
use std::collections::BTreeMap;
use uqa_core::{DocId, FieldName};
use uqa_execution::mutation::{
    identity::MutationIdentifiers,
    publication::{
        DocumentVectors, MutationConstraintDeferrals, MutationHistory, MutationStorage,
        MutationTextIndex, PublicationContext,
    },
};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;
impl Engine {
    pub(crate) fn mutation_publication_context(&self) -> PublicationContext<'_> {
        PublicationContext {
            storage: self,
            text: self,
            history: self,
            deferrals: self,
            identifiers: self,
            catalog: self,
        }
    }
}
impl MutationIdentifiers for Engine {
    fn allocate_next_id(&self, table: &str) -> Result<DocId, SQLError> {
        Engine::allocate_next_id(self, table)
    }
    fn advance_next_id(&self, table: &str, doc_id: DocId) -> Result<(), String> {
        Engine::advance_next_id(self, table, doc_id).map_err(|e| e.to_string())
    }
    fn persist_next_id(&self, table: &str) -> Result<(), String> {
        Engine::persist_next_id(self, table).map_err(|e| e.to_string())
    }
}
impl MutationStorage for Engine {
    fn delete_document(&self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        Engine::delete_document(self, table, doc_id)
    }
    fn insert_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: DocumentVectors,
        known_new: bool,
    ) -> Result<(), SQLError> {
        self.add_prepared_document_with_vector_values(table, doc_id, document, vectors, known_new)
    }
    fn insert_document_deferred_text(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
        vectors: DocumentVectors,
        known_new: bool,
    ) -> Result<(), SQLError> {
        self.add_prepared_document_with_vector_values_deferred_fts(
            table, doc_id, document, vectors, known_new,
        )
    }
    fn rewrite_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        self.rewrite_prepared_document(table, doc_id, document)
    }
}
impl MutationTextIndex for Engine {
    fn text_fields(
        &self,
        table: &str,
        document: &Document,
    ) -> Result<BTreeMap<FieldName, String>, SQLError> {
        self.prepared_document_text_fields(table, document)
    }
    fn add_documents(
        &self,
        table: &str,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> Result<(), SQLError> {
        self.add_prepared_fts_documents(table, documents)
    }
}
impl MutationHistory for Engine {
    fn note_rewrite(
        &self,
        old_table: &str,
        old_doc_id: DocId,
        new_table: &str,
        new_doc_id: DocId,
    ) -> Result<(), SQLError> {
        self.note_row_rewritten_between_tables(old_table, old_doc_id, new_table, new_doc_id)
    }
}
impl MutationConstraintDeferrals for Engine {
    fn inserted(&self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        self.defer_inserted_foreign_key_checks(table, doc_id)
    }
    fn rewritten(
        &self,
        table: &str,
        doc_id: DocId,
        old: Option<&Document>,
        new: &Document,
    ) -> Result<(), SQLError> {
        self.defer_rewritten_foreign_key_checks(table, doc_id, old, new)
    }
}

impl uqa_execution::mutation::staging::MutationCommandRows for Engine {
    fn stage_shared_command_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Option<std::sync::Arc<Document>>,
    ) -> Result<(), SQLError> {
        Engine::stage_shared_command_document(self, table, doc_id, document)
    }
    fn stage_command_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Option<Document>,
    ) -> Result<(), SQLError> {
        Engine::stage_command_document(self, table, doc_id, document)
    }
}
impl Engine {
    pub(crate) fn mutation_staging_context(
        &self,
    ) -> uqa_execution::mutation::staging::MutationStagingContext<'_> {
        uqa_execution::mutation::staging::MutationStagingContext {
            commands: self,
            constraints: self.constraint_execution_context(),
            triggers: self.trigger_execution_context(),
        }
    }
}
