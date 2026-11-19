//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical owners select their own retained membership and supply their original codecs.

use super::tables::TableOwner;
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, ProductionControl},
    DocId,
};
use uqa_storage::{
    document_store::read_document_ids,
    image_io::{ImageOwner, ImageReader},
    memory_capture::MemoryCapture,
    read_control::StorageReadControl,
    vector_index::image::VectorImageSnapshot,
    DocumentStore, InvertedIndex, StorageBackendError, StorageBackendResult,
};

fn absent() -> StorageBackendError {
    StorageBackendError::Other("captured image identity has no physical payload".into())
}

struct Document {
    source: Arc<Budgeted<Arc<dyn DocumentStore>>>,
    id: DocId,
    original: StorageReadControl,
}

impl ImageOwner for Document {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.original.check()?;
        self.source
            .image_document_reader(self.id, control)?
            .ok_or_else(absent)?
            .with_source_control(Some(&self.original), control)
    }
}

pub(super) fn next_document(
    table: &TableOwner,
    after: Option<DocId>,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<(DocId, ImageReader<'static>)>> {
    table.control.check()?;
    let ids = read_document_ids(&***table.document_store, after, 1, control)?;
    let Some(&id) = ids.first() else {
        return Ok(None);
    };
    let reader = ImageReader::from_owner(
        Document {
            source: table.document_store.clone(),
            id,
            original: table.control.clone(),
        },
        control,
    )?;
    table.control.check()?;
    Ok(Some((id, reader)))
}

struct Text {
    source: Arc<Budgeted<Arc<dyn InvertedIndex>>>,
    id: DocId,
    original: StorageReadControl,
}

impl ImageOwner for Text {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.original.check()?;
        self.source
            .image_document_reader(self.id, control)?
            .ok_or_else(absent)?
            .with_source_control(Some(&self.original), control)
    }
}

pub(super) fn next_text(
    table: &TableOwner,
    after: Option<DocId>,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<(DocId, ImageReader<'static>)>> {
    table.control.check()?;
    let Some(id) = table
        .inverted_index
        .next_image_document_id(after, control)?
    else {
        return Ok(None);
    };
    let reader = ImageReader::from_owner(
        Text {
            source: table.inverted_index.clone(),
            id,
            original: table.control.clone(),
        },
        control,
    )?;
    table.control.check()?;
    Ok(Some((id, reader)))
}

struct Vector(Arc<Budgeted<VectorImageSnapshot>>);

impl ImageOwner for Vector {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.0.image_reader(control)
    }
}

pub(super) fn next_vector(
    table: &TableOwner,
    cut: &MemoryCapture,
    after: Option<&str>,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<(Budgeted<String>, ImageReader<'static>)>> {
    table.control.check()?;
    let Some(field) = table.vector_indexes.next_field(after)? else {
        return Ok(None);
    };
    let child = table.vector_indexes.image(field, cut)?;
    let field = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(field)?
    .into_budgeted()
    .expect("retained vector cursor key");
    let reader = ImageReader::from_owner(Vector(child), control)?;
    table.control.check()?;
    Ok(Some((field, reader)))
}

pub(super) type ValueOwner =
    Arc<Budgeted<uqa_execution::catalog::index::value::CapturedValueIndex>>;

pub(super) fn next_value_index(
    table: &TableOwner,
    cut: &MemoryCapture,
    after: Option<&uqa_storage::ValueIndexKey>,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<(Budgeted<uqa_storage::ValueIndexKey>, ValueOwner)>> {
    table.control.check()?;
    let Some(key) = table.value_indexes.next_key(after)? else {
        return Ok(None);
    };
    let source = table.value_indexes.get(key, cut)?.ok_or_else(absent)?;
    let name = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(key.name())?
    .into_budgeted()
    .expect("retained value-index cursor key");
    let (name, memory) = name.into_parts();
    table.control.check()?;
    Ok(Some((
        Budgeted::new(uqa_storage::ValueIndexKey::Index(name), memory),
        source,
    )))
}

struct Evaluated {
    source: ValueOwner,
    id: Option<DocId>,
}

impl ImageOwner for Evaluated {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        match self.id {
            None => self.source.metadata_image_reader(control),
            Some(id) => self
                .source
                .value_image_reader(id, control)?
                .ok_or_else(absent),
        }
    }
}

pub(super) fn value_index_image(
    source: &ValueOwner,
    id: Option<DocId>,
    control: &StorageReadControl,
) -> StorageBackendResult<ImageReader<'static>> {
    ImageReader::from_owner(
        Evaluated {
            source: source.clone(),
            id,
        },
        control,
    )
}

#[cfg(test)]
mod tests;
