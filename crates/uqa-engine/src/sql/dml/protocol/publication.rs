//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed row-change publication carried from mutation application to transaction completion.

use std::collections::BTreeMap;

use uqa_core::DocId;
use uqa_sql::SQLError;

use super::{PreparedDocumentInsert, PreparedMutationAction};
use crate::Engine;

const PREPARED_FTS_BATCH_DOCUMENTS: usize = 4_096;
type PreparedFtsDocuments = Vec<(DocId, BTreeMap<String, String>)>;
type PreparedFtsTables = BTreeMap<String, PreparedFtsDocuments>;

#[derive(Default)]
pub(in crate::sql) struct MutationPublicationBatch {
    fts_tables: PreparedFtsTables,
    fts_document_count: usize,
}

impl MutationPublicationBatch {
    fn push_fts(&mut self, table: String, doc_id: DocId, fields: BTreeMap<String, String>) {
        self.fts_tables
            .entry(table)
            .or_default()
            .push((doc_id, fields));
        self.fts_document_count += 1;
    }

    fn fts_is_full(&self) -> bool {
        self.fts_document_count >= PREPARED_FTS_BATCH_DOCUMENTS
    }

    fn flush_fts(&mut self, engine: &Engine) -> Result<(), SQLError> {
        let tables = std::mem::take(&mut self.fts_tables);
        self.fts_document_count = 0;
        for (table, documents) in tables {
            engine.add_prepared_fts_documents(&table, documents)?;
        }
        Ok(())
    }
}

pub(in crate::sql) fn publish_prepared_mutation_action(
    engine: &Engine,
    action: PreparedMutationAction,
    insert_known_new: bool,
    batch: &mut MutationPublicationBatch,
) -> Result<(), SQLError> {
    match action {
        PreparedMutationAction::Insert(PreparedDocumentInsert {
            table,
            doc_id,
            document,
        }) => {
            let text_fields = engine.prepared_document_text_fields(&table, &document)?;
            let vectors = crate::sql::dml::document_vectors(engine, &table, &document)?;
            engine.add_prepared_document_with_vector_values_deferred_fts(
                &table,
                doc_id,
                document,
                vectors,
                insert_known_new,
            )?;
            engine.defer_inserted_foreign_key_checks(&table, doc_id)?;
            batch.push_fts(table, doc_id, text_fields);
            if batch.fts_is_full() {
                batch.flush_fts(engine)?;
            }
        }
        PreparedMutationAction::Rewrite(mut rewrite) => {
            batch.flush_fts(engine)?;
            crate::sql::dml::apply_validated_prepared_document_rewrite(engine, &mut rewrite)?;
        }
        PreparedMutationAction::Delete(mut delete) => {
            batch.flush_fts(engine)?;
            crate::sql::dml::apply_validated_prepared_document_delete(engine, &mut delete)?;
        }
    }
    Ok(())
}

pub(in crate::sql) fn finish_mutation_publication(
    engine: &Engine,
    batch: &mut MutationPublicationBatch,
) -> Result<(), SQLError> {
    batch.flush_fts(engine)
}

#[derive(Clone, Copy)]
pub(crate) struct TransactionRowChange {
    pub(crate) pending: crate::row_locks::PendingRowChange,
    pub(crate) source_generation: [u8; 16],
    pub(crate) successor_generation: Option<[u8; 16]>,
    pub(crate) query_origin: Option<u64>,
}
