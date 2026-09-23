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
    DocumentStore, InvertedIndex, RetainedDocumentStoreBuilder, StorageBackendError,
    StoredDocument, VectorIndex,
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
    text: RetainedInvertedIndexBuilder,
    vectors: BTreeMap<FieldName, RetainedVectorIndexBuilder>,
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
        RowLayout::new(source_columns, Arc::clone(&schema.columns), control)
            .map_err(|error| snapshot_error("base row layout", &error))?,
        RowLayout::new(&schema.columns, Arc::clone(&schema.columns), control)
            .map_err(|error| snapshot_error("private row layout", &error))?,
        changes,
        control,
    )
    .map_err(|error| snapshot_error("retained documents", &error))?;
    let mut result = SnapshotBuilder::new(schema, control)?;
    let document_count = u64::try_from(
        documents
            .len()
            .map_err(|error| snapshot_error("document count", &error))?,
    )
    .map_err(|_| SQLError::Internal("query snapshot document count overflow".into()))?;
    let fields = schema
        .text_fields
        .iter()
        .chain(schema.vector_dimensions.keys())
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if fields.is_empty() {
        return result.finish(Box::new(documents), document_count);
    }
    let mut after = None;
    loop {
        cancellation.check()?;
        let ids = documents
            .id_page(after, crate::DEFAULT_BATCH_SIZE)
            .map_err(|error| snapshot_error("document ids", &error))?;
        let Some(last) = ids.last().copied() else {
            break;
        };
        after = Some(last);
        result.index_projection(&documents, &ids, &fields, schema)?;
    }
    cancellation.check()?;
    result.finish(Box::new(documents), document_count)
}

/// Reconstruct a selected query view without keeping intermediate corpus-sized document maps. Base rows use their original column identities; evaluated private rows already use the selected schema. Each row moves into an immutable Storage corpus that retains its payload and entry-capacity reservation through the last reader. Source decoding, row adaptation and caller-owned read outputs retain their separate allocation boundaries.
pub fn materialize(
    source: &dyn DocumentStore,
    source_columns: &[ColumnDef],
    schema: &SnapshotSchema<'_>,
    changes: DocumentChanges,
    control: &StorageReadControl,
) -> Result<MaterializedTable, SQLError> {
    let cancellation = control.cancellation();
    cancellation.check()?;
    let layout = RowLayout::new(source_columns, Arc::clone(&schema.columns), control)
        .map_err(|error| snapshot_error("row layout", &error))?;
    let mut result = SnapshotBuilder::new(schema, control)?;
    let mut retained = RetainedDocumentStoreBuilder::new(control);
    let mut after = None;
    loop {
        cancellation.check()?;
        let ids = uqa_storage::document_store::read_document_ids(
            source,
            after,
            crate::DEFAULT_BATCH_SIZE,
            control,
        )
        .map_err(|error| snapshot_error("document ids", &error))?;
        let Some(last) = ids.last().copied() else {
            break;
        };
        after = Some(last);
        let mut selected = uqa_core::memory::BudgetedVec::new(control.memory());
        for id in ids.iter().copied() {
            cancellation.check()?;
            if !changes.contains_change(id) {
                selected
                    .push(id)
                    .map_err(|error| snapshot_error("document selection", &error.into()))?;
            }
        }
        if selected.is_empty() {
            continue;
        }
        let documents = source
            .get_stored_many(&selected)
            .map_err(|error| snapshot_error("documents", &error))?;
        for (id, document) in documents {
            cancellation.check()?;
            let document = layout.adapt_base(document)?;
            result.insert(&mut retained, id, document, schema)?;
        }
    }
    for change in changes.into_rows() {
        cancellation.check()?;
        let (id, document) = change.map_err(|error| snapshot_error("private documents", &error))?;
        if let Some(document) = document {
            result.insert(
                &mut retained,
                id,
                layout.complete_private(document)?,
                schema,
            )?;
        }
    }
    cancellation.check()?;
    let documents = retained
        .finish()
        .map_err(|error| snapshot_error("documents", &error))?;
    let document_count = u64::try_from(
        documents
            .len()
            .map_err(|error| snapshot_error("document count", &error))?,
    )
    .map_err(|_| SQLError::Internal("query snapshot document count overflow".into()))?;
    result.finish(Box::new(documents), document_count)
}

pub fn empty(
    schema: &SnapshotSchema<'_>,
    control: &StorageReadControl,
) -> Result<MaterializedTable, SQLError> {
    let documents = RetainedDocumentStoreBuilder::new(control)
        .finish()
        .map_err(|error| snapshot_error("documents", &error))?;
    SnapshotBuilder::new(schema, control)?.finish(Box::new(documents), 0)
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
            text,
            vectors,
            control: control.clone(),
        })
    }

    fn finish(
        self,
        documents: Box<dyn DocumentStore>,
        document_count: u64,
    ) -> Result<MaterializedTable, SQLError> {
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
            documents,
            text: Box::new(
                self.text
                    .finish()
                    .map_err(|error| snapshot_error("inverted index", &error))?,
            ),
            vectors,
            document_count,
        })
    }

    fn insert(
        &mut self,
        documents: &mut RetainedDocumentStoreBuilder,
        id: DocId,
        document: StoredDocument,
        schema: &SnapshotSchema<'_>,
    ) -> Result<(), SQLError> {
        self.index_fields(id, |field| document.fields().get(field), schema)?;
        documents
            .add_document(id, document)
            .map_err(|error| snapshot_error("document", &error))
    }

    fn index_projection(
        &mut self,
        documents: &dyn DocumentStore,
        ids: &[DocId],
        fields: &[&str],
        schema: &SnapshotSchema<'_>,
    ) -> Result<(), SQLError> {
        let mut failure = None;
        let visited = documents.for_each_fields_multi_ref_with_presence(
            ids,
            fields,
            &mut |id, present, values| {
                if !present {
                    return true;
                }
                let indexed = self.index_fields(
                    id,
                    |field| fields.binary_search(&field).ok().map(|slot| values[slot]),
                    schema,
                );
                if let Err(error) = indexed {
                    failure = Some(error);
                    return false;
                }
                true
            },
        );
        // Keep the first consumer failure when a provider notices cancellation while unwinding its borrowed projection.
        if let Some(error) = failure {
            return Err(error);
        }
        visited.map_err(|error| snapshot_error("index fields", &error))
    }

    fn index_fields<'a>(
        &mut self,
        id: DocId,
        mut value_for: impl FnMut(&str) -> Option<&'a Value>,
        schema: &SnapshotSchema<'_>,
    ) -> Result<(), SQLError> {
        let fields = schema
            .text_fields
            .iter()
            .filter_map(|field| match value_for(field) {
                Some(Value::Str(value)) => Some((field.as_str(), value.as_str())),
                _ => None,
            });
        self.text
            .add_document(id, fields)
            .map_err(|error| snapshot_error("inverted index", &error))?;
        for (field, index) in &mut self.vectors {
            let Some(value) = value_for(field) else {
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
