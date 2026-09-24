//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-table row adaptation and reconstruction from a retained base and evaluated private rows.

use std::sync::Arc;
use uqa_core::memory::{BudgetedMap, BudgetedVec, MemoryReservation};
use uqa_core::{DocId, FieldName, Value};
use uqa_sql::{ast::ColumnDef, schema::retention::RetainedColumns, ColumnType, SQLError};
use uqa_storage::inverted_index::RetainedInvertedIndexBuilder;
use uqa_storage::{
    read_control::StorageReadControl,
    vector_index::{RetainedVectorIndexBuilder, RetainedVectorIndexesBuilder, VectorIndexes},
};
use uqa_storage::{
    DocumentStore, InvertedIndex, RetainedDocumentStoreBuilder, StorageBackendError,
};

mod documents;
mod layout;
mod vector_metadata;
use super::document_changes::DocumentChanges;
use layout::RowLayout;
pub use vector_metadata::{retain_vector_indexes, VectorDimensions};

#[cfg(test)]
mod tests;

/// The selected catalog supplies immutable column definitions, field revisions and registered vector dimensions. The reconstruction does not consult live catalog state.
pub struct SnapshotSchema<'a> {
    pub columns: Arc<Vec<ColumnDef>>,
    pub text_fields: &'a [FieldName],
    pub text_revisions: &'a dyn InvertedIndex,
    pub vector_dimensions: &'a dyn VectorDimensions,
}

pub struct MaterializedTable {
    pub documents: Box<dyn DocumentStore>,
    pub text: Box<dyn InvertedIndex>,
    pub vectors: VectorIndexes,
    pub document_count: u64,
}

struct SnapshotBuilder {
    text: RetainedInvertedIndexBuilder,
    vectors: BudgetedVec<(FieldName, RetainedVectorIndexBuilder, MemoryReservation)>,
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
    let columns = RetainedColumns::capture(&schema.columns, control.memory(), cancellation)?;
    let documents = documents::RetainedDocuments::new(
        source,
        RowLayout::new(source_columns, columns.clone(), control)
            .map_err(|error| snapshot_error("base row layout", &error))?,
        RowLayout::new(&schema.columns, columns, control)
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
    let fields = index_fields(schema, control)?;
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

fn index_fields<'a>(
    schema: &'a SnapshotSchema<'_>,
    control: &StorageReadControl,
) -> Result<BudgetedVec<&'a str>, SQLError> {
    control.cancellation().check()?;
    let mut selected = BudgetedMap::new(control.memory());
    for field in schema.text_fields {
        control.cancellation().check()?;
        selected
            .insert(field.as_str(), ())
            .map_err(|error| snapshot_error("index field selection", &error.into()))?;
    }
    schema.vector_dimensions.visit(&mut |field, _| {
        control.cancellation().check()?;
        selected
            .insert(field, ())
            .map_err(|error| snapshot_error("index field selection", &error.into()))?;
        Ok(())
    })?;
    let mut fields = BudgetedVec::new(control.memory());
    fields
        .reserve(selected.len())
        .map_err(|error| snapshot_error("index field projection", &error.into()))?;
    for (field, ()) in selected.iter() {
        control.cancellation().check()?;
        fields
            .push(*field)
            .map_err(|error| snapshot_error("index field projection", &error.into()))?;
    }
    Ok(fields)
}

/// Copy a mutable source into a controlled immutable corpus, or share an already retained source. Provider row pages keep their payload reservations through corpus adoption. The existing read adapter maps original column identities and shares evaluated private rows and selected definitions; capture does not construct defaults or unrequested generated values for every row. Text/vector reconstruction evaluates its requested fields. Owned read outputs retain their separate producer boundaries.
pub fn materialize(
    source: &dyn DocumentStore,
    source_columns: &[ColumnDef],
    schema: &SnapshotSchema<'_>,
    changes: DocumentChanges,
    control: &StorageReadControl,
) -> Result<MaterializedTable, SQLError> {
    let cancellation = control.cancellation();
    cancellation.check()?;
    if let Some(source) = source
        .retained_snapshot()
        .map_err(|error| snapshot_error("retained documents", &error))?
    {
        return retain(source, source_columns, schema, changes, control);
    }
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
        let (documents, _page_memory) =
            uqa_storage::document_store::read_stored_documents(source, &selected, control)
                .map_err(|error| snapshot_error("documents", &error))?
                .into_parts();
        for (id, document) in selected.iter().copied().zip(documents) {
            cancellation.check()?;
            if let Some(document) = document {
                retained
                    .add_retained_document(id, document)
                    .map_err(|error| snapshot_error("document", &error))?;
            }
        }
    }
    cancellation.check()?;
    let documents = retained
        .finish()
        .map_err(|error| snapshot_error("documents", &error))?;
    retain(
        Arc::new(documents),
        source_columns,
        schema,
        changes,
        control,
    )
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
        let revisions = schema.text_fields.iter().map(|field| {
            control.check()?;
            Ok((
                field.as_str(),
                schema.text_revisions.index_analyzer_revision(field)?,
                schema.text_revisions.search_analyzer_revision(field)?,
            ))
        });
        let text = RetainedInvertedIndexBuilder::from_revisions(
            schema
                .text_revisions
                .default_analyzer_binding()
                .map_err(|error| snapshot_error("default analyzer binding", &error))?,
            revisions,
            control,
        )
        .map_err(|error| snapshot_error("inverted index", &error))?;
        let mut vectors = BudgetedVec::new(control.memory());
        schema.vector_dimensions.visit(&mut |field, dimensions| {
            control.cancellation().check()?;
            vectors
                .reserve(1)
                .map_err(|error| snapshot_error("vector metadata", &error.into()))?;
            let (name, memory) = vector_metadata::copy_field(field, control)?;
            vectors
                .push((
                    name,
                    RetainedVectorIndexBuilder::new(dimensions, control),
                    memory,
                ))
                .map_err(|error| snapshot_error("vector metadata", &error.into()))?;
            Ok(())
        })?;
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
        let (builders, _builder_memory) = self.vectors.into_parts();
        let mut vectors = RetainedVectorIndexesBuilder::new(&self.control);
        for (field, index, memory) in builders {
            self.control.cancellation().check()?;
            let index = index
                .finish()
                .map_err(|error| snapshot_error("vector index", &error))?;
            vectors
                .insert_admitted(field, index, memory)
                .map_err(|error| snapshot_error("vector index metadata", &error))?;
        }
        Ok(MaterializedTable {
            documents,
            text: Box::new(
                self.text
                    .finish()
                    .map_err(|error| snapshot_error("inverted index", &error))?,
            ),
            vectors: vectors
                .finish()
                .map_err(|error| snapshot_error("vector indexes", &error))?,
            document_count,
        })
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
        for (field, index, _) in self.vectors.iter_mut() {
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
