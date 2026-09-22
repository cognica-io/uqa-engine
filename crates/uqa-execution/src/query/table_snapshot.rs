//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-table row adaptation and reconstruction from a retained base and evaluated private rows.

use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_analysis::Analyzer;
use uqa_core::{CancellationToken, DocId, FieldName, Value};
use uqa_sql::{ast::ColumnDef, ColumnType, SQLError};
use uqa_storage::{
    DocumentStore, InvertedIndex, MemoryDocumentStore, MemoryInvertedIndex, MemoryVectorIndex,
    StoredDocument, VectorIndex,
};

mod documents;
mod layout;
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

/// Retain the immutable base instead of copying its documents into another complete store. Private replacements and row-layout metadata are shared by nested views, and projected reads avoid unrelated fields. Text/vector reconstruction still retains its resulting indexes.
pub fn retain(
    source: Arc<dyn DocumentStore>,
    source_columns: &[ColumnDef],
    schema: &SnapshotSchema<'_>,
    changes: BTreeMap<DocId, Option<StoredDocument>>,
    cancellation: &CancellationToken,
) -> Result<MaterializedTable, SQLError> {
    cancellation.check()?;
    let documents = documents::RetainedDocuments::new(
        source,
        RowLayout::new(source_columns, Arc::clone(&schema.columns)),
        changes,
        cancellation,
    )
    .map_err(|error| snapshot_error("retained documents", &error))?;
    let mut result = empty(schema)?;
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
        return Ok(result);
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
    Ok(result)
}

/// Reconstruct a selected query view without keeping intermediate corpus-sized document maps. Base rows use their original column identities; evaluated private rows already use the selected schema. Each row moves into its final store after its index inputs are extracted. The resulting memory stores still retain the complete selected view.
pub fn materialize(
    source: &dyn DocumentStore,
    source_columns: &[ColumnDef],
    schema: &SnapshotSchema<'_>,
    changes: BTreeMap<DocId, Option<StoredDocument>>,
    cancellation: &CancellationToken,
) -> Result<MaterializedTable, SQLError> {
    cancellation.check()?;
    let layout = RowLayout::new(source_columns, Arc::clone(&schema.columns));
    let mut result = empty(schema)?;
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
            .filter(|id| !changes.contains_key(id))
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
    for (id, document) in changes {
        cancellation.check()?;
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
    Ok(result)
}

pub fn empty(schema: &SnapshotSchema<'_>) -> Result<MaterializedTable, SQLError> {
    let mut text = MemoryInvertedIndex::new(schema.analyzer.clone());
    for field in schema.text_fields {
        text.set_field_analyzer_revisions(
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
        .map_err(|error| snapshot_error("field analyzer revisions", &error))?;
    }
    let vectors = schema
        .vector_dimensions
        .iter()
        .map(|(field, dimensions)| {
            (
                field.clone(),
                Box::new(MemoryVectorIndex::new(*dimensions)) as Box<dyn VectorIndex>,
            )
        })
        .collect();
    Ok(MaterializedTable {
        documents: Box::new(MemoryDocumentStore::new()),
        text: Box::new(text),
        vectors,
        document_count: 0,
    })
}

impl MaterializedTable {
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
                Some(Value::Str(value)) => Some((field.clone(), value.clone())),
                _ => None,
            })
            .collect();
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
            let vectors = uqa_sql::assignment::vectors::index_vectors_for_type(value, ty)?;
            index
                .add_many(id, vectors)
                .map_err(|error| snapshot_error("vector index", &error))?;
        }
        Ok(())
    }
}

fn snapshot_error(component: &str, error: &impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("construct query {component} snapshot: {error}"))
}
