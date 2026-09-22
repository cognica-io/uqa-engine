//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-table row adaptation and reconstruction from a retained base and evaluated private rows.

use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_analysis::Analyzer;
use uqa_core::{DocId, FieldName, Value};
use uqa_sql::{ast::ColumnDef, ColumnType, SQLError};
use uqa_storage::inverted_index::{AnalyzerBindings, RetainedInvertedIndexBuilder};
use uqa_storage::{read_control::StorageReadControl, vector_index::RetainedVectorIndexBuilder};
use uqa_storage::{
    DocumentStore, InvertedIndex, MemoryDocumentStore, StorageBackendError, StoredDocument,
    VectorIndex,
};

mod documents;
mod layout;
use super::document_changes::DocumentChanges;
use layout::RowLayout;

#[cfg(test)]
mod tests;

/// The selected catalog supplies immutable column definitions, field revisions and registered vector dimensions. The reconstruction does not consult live catalog state.
pub struct SnapshotSchema<'a> {
    pub columns: Arc<Vec<ColumnDef>>,
    pub analyzer: &'a Analyzer,
    pub text_fields: &'a [FieldName],
    pub text_revisions: &'a dyn InvertedIndex,
    pub vector_dimensions: BTreeMap<FieldName, u32>,
}

pub struct MaterializedTable {
    pub documents: Box<dyn DocumentStore>,
    pub text: Box<dyn InvertedIndex>,
    pub vectors: BTreeMap<FieldName, Box<dyn VectorIndex>>,
    pub document_count: u64,
}

struct SnapshotBuilder {
    documents: Box<dyn DocumentStore>,
    text: RetainedInvertedIndexBuilder,
    vectors: BTreeMap<FieldName, RetainedVectorIndexBuilder>,
    document_count: u64,
    control: StorageReadControl,
}

/// Retain the immutable base instead of copying its documents into another complete store. Private replacements and row-layout metadata are shared by nested views, and projected reads avoid unrelated fields. Text/vector reconstruction still retains its resulting indexes.
pub fn retain(
    source: Arc<dyn DocumentStore>,
    source_columns: &[ColumnDef],
    schema: &SnapshotSchema<'_>,
    changes: DocumentChanges,
    control: &StorageReadControl,
) -> Result<MaterializedTable, SQLError> {
    let cancellation = control.cancellation();
    cancellation.check()?;
    let documents = documents::RetainedDocuments::new(
        source,
        RowLayout::new(source_columns, Arc::clone(&schema.columns)),
        RowLayout::new(&schema.columns, Arc::clone(&schema.columns)),
        changes,
        cancellation,
    )
    .map_err(|error| snapshot_error("retained documents", &error))?;
    let mut result = SnapshotBuilder::new(schema, control)?;
    result.document_count = u64::try_from(
        documents
            .len()
            .map_err(|error| snapshot_error("document count", &error))?,
    )
    .map_err(|_| SQLError::Internal("query snapshot document count overflow".into()))?;
    result.documents = Box::new(documents);
    let fields = schema
        .text_fields
        .iter()
        .chain(schema.vector_dimensions.keys())
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if fields.is_empty() {
        return result.finish();
    }
    let mut after = None;
    loop {
        cancellation.check()?;
        let ids = result
            .documents
            .next_doc_ids(after, crate::DEFAULT_BATCH_SIZE)
            .map_err(|error| snapshot_error("document ids", &error))?;
        let Some(last) = ids.last().copied() else {
            break;
        };
        after = Some(last);
        let rows = result
            .documents
            .get_fields_multi(&ids, &fields)
            .map_err(|error| snapshot_error("index fields", &error))?;
        for (id, values) in rows {
            cancellation.check()?;
            let values = fields
                .iter()
                .zip(values)
                .map(|(field, value)| ((*field).to_string(), value))
                .collect();
            result.index_fields(id, &values, schema)?;
        }
    }
    cancellation.check()?;
    result.finish()
}

/// Reconstruct a selected query view without keeping intermediate corpus-sized document maps. Base rows use their original column identities; evaluated private rows already use the selected schema. Each row moves into its final store after its index inputs are extracted. The resulting memory stores still retain the complete selected view.
pub fn materialize(
    source: &dyn DocumentStore,
    source_columns: &[ColumnDef],
    schema: &SnapshotSchema<'_>,
    changes: DocumentChanges,
    control: &StorageReadControl,
) -> Result<MaterializedTable, SQLError> {
    let cancellation = control.cancellation();
    cancellation.check()?;
    let layout = RowLayout::new(source_columns, Arc::clone(&schema.columns));
    let mut result = SnapshotBuilder::new(schema, control)?;
    let mut after = None;
    loop {
        cancellation.check()?;
        let ids = source
            .next_doc_ids(after, crate::DEFAULT_BATCH_SIZE)
            .map_err(|error| snapshot_error("document ids", &error))?;
        let Some(last) = ids.last().copied() else {
            break;
        };
        if after.is_some_and(|after| ids[0] <= after)
            || ids.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(SQLError::Internal(
                "query snapshot document page did not advance in id order".into(),
            ));
        }
        after = Some(last);
        let selected = ids
            .into_iter()
            .filter(|id| !changes.contains_change(*id))
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let documents = source
            .get_stored_many(&selected)
            .map_err(|error| snapshot_error("documents", &error))?;
        for (id, document) in documents {
            cancellation.check()?;
            let document = layout.adapt_base(document)?;
            result.insert(id, document, schema)?;
        }
    }
    for change in changes.into_rows() {
        cancellation.check()?;
        let (id, document) = change.map_err(|error| snapshot_error("private documents", &error))?;
        if let Some(document) = document {
            result.insert(id, layout.complete_private(document)?, schema)?;
        }
    }
    cancellation.check()?;
    result.document_count = u64::try_from(
        result
            .documents
            .len()
            .map_err(|error| snapshot_error("document count", &error))?,
    )
    .map_err(|_| SQLError::Internal("query snapshot document count overflow".into()))?;
    result.finish()
}

pub fn empty(
    schema: &SnapshotSchema<'_>,
    control: &StorageReadControl,
) -> Result<MaterializedTable, SQLError> {
    SnapshotBuilder::new(schema, control)?.finish()
}

impl SnapshotBuilder {
    fn new(schema: &SnapshotSchema<'_>, control: &StorageReadControl) -> Result<Self, SQLError> {
        control.cancellation().check()?;
        let mut bindings = AnalyzerBindings::new(schema.analyzer.clone());
        for field in schema.text_fields {
            bindings
                .bind_revisions(
                    field,
                    schema
                        .text_revisions
                        .index_analyzer_revision(field)
                        .map_err(|error| snapshot_error("index analyzer revision", &error))?,
                    schema
                        .text_revisions
                        .search_analyzer_revision(field)
                        .map_err(|error| snapshot_error("search analyzer revision", &error))?,
                )
                .map_err(|error| snapshot_error("field analyzer revisions", &error.into()))?;
        }
        let text = RetainedInvertedIndexBuilder::new(bindings, control)
            .map_err(|error| snapshot_error("inverted index", &error))?;
        let vectors = schema
            .vector_dimensions
            .iter()
            .map(|(field, dimensions)| {
                (
                    field.clone(),
                    RetainedVectorIndexBuilder::new(*dimensions, control),
                )
            })
            .collect();
        Ok(Self {
            documents: Box::new(MemoryDocumentStore::new()),
            text,
            vectors,
            document_count: 0,
            control: control.clone(),
        })
    }

    fn finish(self) -> Result<MaterializedTable, SQLError> {
        let vectors = self
            .vectors
            .into_iter()
            .map(|(field, index)| {
                let index = index
                    .finish()
                    .map_err(|error| snapshot_error("vector index", &error))?;
                Ok((field, Box::new(index) as Box<dyn VectorIndex>))
            })
            .collect::<Result<_, SQLError>>()?;
        Ok(MaterializedTable {
            documents: self.documents,
            text: Box::new(
                self.text
                    .finish()
                    .map_err(|error| snapshot_error("inverted index", &error))?,
            ),
            vectors,
            document_count: self.document_count,
        })
    }

    fn insert(
        &mut self,
        id: DocId,
        document: StoredDocument,
        schema: &SnapshotSchema<'_>,
    ) -> Result<(), SQLError> {
        self.index_fields(id, document.fields(), schema)?;
        self.documents
            .put_stored(id, document)
            .map_err(|error| snapshot_error("document", &error))
    }

    fn index_fields(
        &mut self,
        id: DocId,
        document: &uqa_storage::document_store::Document,
        schema: &SnapshotSchema<'_>,
    ) -> Result<(), SQLError> {
        let fields = schema
            .text_fields
            .iter()
            .filter_map(|field| match document.get(field) {
                Some(Value::Str(value)) => Some((field.as_str(), value.as_str())),
                _ => None,
            });
        self.text
            .add_document(id, fields)
            .map_err(|error| snapshot_error("inverted index", &error))?;
        for (field, index) in &mut self.vectors {
            let Some(value) = document.get(field) else {
                continue;
            };
            let fallback = ColumnType::Vector(index.dimensions());
            let ty = schema
                .columns
                .iter()
                .find(|column| column.name == *field)
                .map(|column| &column.ty)
                .filter(|ty| matches!(ty, ColumnType::Tensor(_) | ColumnType::Vector(_)))
                .unwrap_or(&fallback);
            let vectors = uqa_sql::assignment::vectors::index_vectors_for_type_budgeted(
                value,
                ty,
                self.control.memory(),
                &mut || self.control.cancellation().check(),
            )?;
            index
                .add_document(id, vectors)
                .map_err(|error| snapshot_error("vector index", &error))?;
        }
        Ok(())
    }
}

fn snapshot_error(component: &str, error: &StorageBackendError) -> SQLError {
    crate::storage_errors::storage_error(&format!("construct query {component} snapshot"), error)
}
