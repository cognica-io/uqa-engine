//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native table image input joins evaluated Storage owners with named SQL value indexes.

use super::index::value::{
    MemoryValueIndexesBuilder, ValueIndexImageBuilder, ValueIndexImageMetadata, ValueIndexes,
};
use std::{io::Read, sync::Arc};
use uqa_core::memory::{Budgeted, ProductionControl};
use uqa_storage::{
    backup::DecodedMemoryTableUnit,
    diskann_index::build::DiskANNTemporaryBudget,
    document_store::{image::read_document_image, MemoryDocumentStore, MemoryDocumentStoreBuilder},
    image_io::ImageInput,
    inverted_index::{AnalyzerDefault, RetainedInvertedIndexBuilder},
    memory_capture::MemoryCaptureRegistry,
    read_control::StorageReadControl,
    vector_index::{
        image::read_vector_image_with_temporary, MemoryVectorIndexesBuilder, VectorIndexes,
    },
    MemoryInvertedIndex, StorageBackendError, StorageBackendResult,
};

/// Privately reconstructed writable data. Definitions, image membership, logical catalog closure and publication are separate owners; no SQL, analyzer or vector construction runs during this assembly.
pub struct MemoryTableData {
    pub documents: MemoryDocumentStore,
    pub text: MemoryInvertedIndex,
    pub vectors: VectorIndexes,
    pub values: ValueIndexes,
}

/// The image reader supplies ordered table components. A failed component consumes the complete candidate, including every previously decoded row and index registration.
pub struct MemoryTableDataInput {
    documents: MemoryDocumentStoreBuilder,
    text: RetainedInvertedIndexBuilder,
    vectors: MemoryVectorIndexesBuilder,
    values: MemoryValueIndexesBuilder,
    selected: Option<(Budgeted<String>, ValueIndexImageBuilder)>,
    control: StorageReadControl,
    value_limit: usize,
    temporary: DiskANNTemporaryBudget,
}

impl MemoryTableDataInput {
    pub fn new<'a>(
        default: AnalyzerDefault,
        fields: impl IntoIterator<
            Item = StorageBackendResult<(
                &'a str,
                Arc<uqa_analysis::CompiledAnalyzer>,
                Arc<uqa_analysis::CompiledAnalyzer>,
            )>,
        >,
        registry: &MemoryCaptureRegistry,
        control: &StorageReadControl,
        value_limit: usize,
        temporary: &DiskANNTemporaryBudget,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            documents: MemoryDocumentStoreBuilder::new(control),
            text: RetainedInvertedIndexBuilder::from_revisions(default, fields, control)?,
            vectors: MemoryVectorIndexesBuilder::new(registry, control),
            values: MemoryValueIndexesBuilder::new(registry, control),
            selected: None,
            control: control.clone(),
            value_limit,
            temporary: temporary.clone(),
        })
    }

    /// Consume one complete native data image. The surrounding Storage image reader owns address order and required metadata admission; native component decoders reject duplicate identities and invalid or trailing payload bytes.
    pub fn read(
        mut self,
        unit: &DecodedMemoryTableUnit,
        reader: &mut dyn Read,
    ) -> StorageBackendResult<Self> {
        self.control.check()?;
        match unit {
            DecodedMemoryTableUnit::Metadata(_) => {
                return Err(invalid("table metadata is not a data component"))
            }
            DecodedMemoryTableUnit::Document(id) => {
                let document = read_document_image(reader, &self.control)?;
                ImageInput {
                    reader,
                    control: &self.control,
                }
                .end()?;
                self.documents = self.documents.add_document(*id, document)?;
            }
            DecodedMemoryTableUnit::Text(id) => self.text.add_image_document(*id, reader)?,
            DecodedMemoryTableUnit::Vector(field) => {
                let index =
                    read_vector_image_with_temporary(reader, &self.control, &self.temporary)?;
                let field = self.name(field)?;
                self.vectors = self.vectors.add_index(field, index)?;
            }
            DecodedMemoryTableUnit::ValueIndex {
                name,
                document: None,
            } => {
                self = self.finish_selected()?;
                let metadata = ValueIndexImageMetadata::read_image(reader, &self.control)?;
                self.selected = Some((
                    self.name(name)?,
                    ValueIndexImageBuilder::new(metadata, self.value_limit, &self.control)
                        .map_err(sql_error)?,
                ));
            }
            DecodedMemoryTableUnit::ValueIndex {
                name,
                document: Some(id),
            } => {
                let (selected_name, index) = self
                    .selected
                    .take()
                    .ok_or_else(|| invalid("evaluated value has no index definition"))?;
                if selected_name.as_str() != &**name {
                    return Err(invalid("evaluated value belongs to another index"));
                }
                let mut input = ImageInput {
                    reader,
                    control: &self.control,
                };
                let value = input.value()?;
                input.end()?;
                self.selected = Some((
                    selected_name,
                    index.add_value(*id, value).map_err(sql_error)?,
                ));
            }
        }
        self.control.check()?;
        Ok(self)
    }

    pub fn finish(self) -> StorageBackendResult<MemoryTableData> {
        let value = self.finish_selected()?;
        value.control.check()?;
        Ok(MemoryTableData {
            documents: value.documents.finish()?,
            text: value.text.finish_writable()?,
            vectors: value.vectors.finish()?,
            values: value.values.finish()?,
        })
    }

    fn finish_selected(mut self) -> StorageBackendResult<Self> {
        if let Some((name, index)) = self.selected.take() {
            self.values = self
                .values
                .add_index(name, index.finish().map_err(sql_error)?)?;
        }
        Ok(self)
    }

    fn name(&self, name: &str) -> StorageBackendResult<Budgeted<String>> {
        let production = ProductionControl::new(
            self.control.memory(),
            self.control.cancellation(),
            self.control.cancellation(),
        );
        Ok(production
            .copy_text(name)?
            .into_budgeted()
            .expect("admitted image registration name"))
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    StorageBackendError::backend("table evaluated image", error)
}

#[cfg(test)]
mod tests;
