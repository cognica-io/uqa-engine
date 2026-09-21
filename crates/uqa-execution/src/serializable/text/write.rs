//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture evaluated logical keys inside the provider, then publish intents outside its non-reentrant mutation scope.

use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{memory::BudgetedVec, DocId, FieldName};
use uqa_sql::{ast::ColumnDef, SQLError};
use uqa_storage::{
    inverted_index::{InvertedIndexChange, InvertedIndexChangeVisitor},
    mvcc::{SerializableKeySpace, SerializablePredicate},
    InvertedIndex, StorageBackendResult,
};

use super::{TextObservation, DOCUMENT, STATISTICS};
use crate::{
    serializable::{SerializableRelationRead, SerializableWrites},
    storage_errors::storage_error,
};

enum Mutation {
    Point(DocId, BTreeMap<FieldName, String>),
    Batch(Vec<(DocId, BTreeMap<FieldName, String>)>),
    Delete(DocId),
}

impl Mutation {
    fn apply(
        self,
        index: &mut dyn InvertedIndex,
        visit: Option<&mut InvertedIndexChangeVisitor<'_>>,
    ) -> StorageBackendResult<()> {
        match (self, visit) {
            (Self::Point(doc, fields), Some(visit)) => {
                index.try_add_documents_observed(vec![(doc, fields)], visit)
            }
            (Self::Batch(documents), Some(visit)) => {
                index.try_add_documents_observed(documents, visit)
            }
            (Self::Delete(doc), Some(visit)) => index.try_remove_document_observed(doc, visit),
            (Self::Point(doc, fields), None) => index.add_document(doc, fields),
            (Self::Batch(documents), None) => index.try_add_documents(documents),
            (Self::Delete(doc), None) => index.remove_document(doc),
        }
    }
}

pub fn add_document(
    writes: &dyn SerializableWrites,
    table: &str,
    columns: Arc<Vec<ColumnDef>>,
    index: &mut dyn InvertedIndex,
    doc: DocId,
    fields: BTreeMap<FieldName, String>,
) -> Result<(), SQLError> {
    mutate(writes, table, columns, index, Mutation::Point(doc, fields))
}

pub fn add_documents(
    writes: &dyn SerializableWrites,
    table: &str,
    columns: Arc<Vec<ColumnDef>>,
    index: &mut dyn InvertedIndex,
    documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
) -> Result<(), SQLError> {
    mutate(writes, table, columns, index, Mutation::Batch(documents))
}

pub fn remove_document(
    writes: &dyn SerializableWrites,
    table: &str,
    columns: Arc<Vec<ColumnDef>>,
    index: &mut dyn InvertedIndex,
    doc: DocId,
) -> Result<(), SQLError> {
    mutate(writes, table, columns, index, Mutation::Delete(doc))
}

fn mutate(
    writes: &dyn SerializableWrites,
    table: &str,
    columns: Arc<Vec<ColumnDef>>,
    index: &mut dyn InvertedIndex,
    mutation: Mutation,
) -> Result<(), SQLError> {
    let Some(read) = SerializableRelationRead::for_mutation(writes, table)? else {
        return mutation
            .apply(index, None)
            .map_err(|error| storage_error("mutate text index", &error));
    };
    let observation = TextObservation { read, columns };
    let mut keys = BudgetedVec::new(observation.read.control.memory());
    mutation
        .apply(
            index,
            Some(&mut |change| {
                observation.read.control.check()?;
                keys.reserve(1)?;
                keys.push(observation.write_key(change)?)?;
                Ok(())
            }),
        )
        .map_err(|error| storage_error("capture evaluated text changes", &error))?;
    // The native mutation gate is now released. The original statement/savepoint still owns both the private records and every intent registered below.
    let session = writes.serializable_session().ok_or_else(|| {
        SQLError::Internal("serializable text writer lost its original session".into())
    })?;
    for key in keys.iter() {
        observation
            .read
            .control
            .check()
            .map_err(|error| storage_error("publish text intent", &error))?;
        session
            .observe_serializable_write(SerializablePredicate::point(
                observation.read.object,
                SerializableKeySpace::Text,
                key,
            ))
            .map_err(|error| storage_error("publish text intent", &error))?;
    }
    Ok(())
}

impl TextObservation {
    fn write_key(&self, change: InvertedIndexChange<'_>) -> StorageBackendResult<BudgetedVec<u8>> {
        match change {
            InvertedIndexChange::Posting {
                doc_id,
                field,
                term,
            } => {
                let mut key = self.term_key(field, term)?;
                key.extend_from_slice(&doc_id.to_be_bytes())?;
                Ok(key)
            }
            InvertedIndexChange::Document { doc_id, field } => {
                let mut key = self.field_key(DOCUMENT, field)?;
                key.extend_from_slice(&doc_id.to_be_bytes())?;
                Ok(key)
            }
            InvertedIndexChange::FieldStatistics { field } => self.field_key(STATISTICS, field),
        }
    }
}
