//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native table image input joins evaluated Storage owners with named SQL value indexes.

use super::index::value::{MemoryValueIndexesBuilder, ValueIndexes};
use std::{io::Read, sync::Arc};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    DocId,
};
use uqa_storage::{
    backup::{DecodedMemoryTableUnit, MemoryImageName},
    diskann_index::build::DiskANNTemporaryBudget,
    document_store::{MemoryDocumentStore, MemoryDocumentStoreBuilder},
    inverted_index::{AnalyzerDefault, RetainedInvertedIndexBuilder},
    memory_capture::MemoryCaptureRegistry,
    read_control::StorageReadControl,
    vector_index::{MemoryVectorIndexesBuilder, VectorIndexes},
    MemoryInvertedIndex, StorageBackendError, StorageBackendResult,
};

mod value_input;
use value_input::ValueInput;

/// Privately reconstructed writable data. Definitions, image membership, logical catalog closure and publication are separate owners; no SQL, analyzer or vector construction runs during this assembly.
pub struct MemoryTableData {
    pub documents: MemoryDocumentStore,
    pub text: MemoryInvertedIndex,
    pub vectors: VectorIndexes,
    pub values: ValueIndexes,
}

/// The image reader supplies ordered table components. A failed component consumes the complete candidate, including every previously decoded row and index registration.
pub struct MemoryTableDataInput {
    documents: DocumentInput<MemoryDocumentStoreBuilder>,
    text: DocumentInput<RetainedInvertedIndexBuilder>,
    vectors: VectorInput,
    values: ValueInput,
    control: StorageReadControl,
    value_limit: usize,
    temporary: DiskANNTemporaryBudget,
}

enum VectorInput {
    Ready(MemoryVectorIndexesBuilder),
    Reading {
        field: MemoryImageName,
        input: ImageDecoder<MemoryVectorIndexesBuilder, StorageBackendError>,
    },
}

enum DocumentInput<T> {
    Ready(T),
    Reading {
        id: DocId,
        input: ImageDecoder<T, StorageBackendError>,
    },
}

impl<T> DocumentInput<T> {
    fn read(
        self,
        id: DocId,
        reader: &mut dyn Read,
        window: ImageDecodeWindow,
        begin: impl FnOnce(T, DocId) -> StorageBackendResult<ImageDecoder<T, StorageBackendError>>,
    ) -> StorageBackendResult<(Self, bool)> {
        let mut input = match self {
            Self::Ready(documents) => begin(documents, id)?,
            Self::Reading {
                id: selected,
                input,
            } if id == selected => input,
            Self::Reading { .. } => {
                return Err(invalid("document input changed its selected identity"))
            }
        };
        Ok(match input.advance(reader, window)? {
            Some(documents) => (Self::Ready(documents), true),
            None => (Self::Reading { id, input }, false),
        })
    }
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
        let text = RetainedInvertedIndexBuilder::from_revisions(default, fields, control)?;
        Ok(Self::with_text(
            text,
            registry,
            control,
            value_limit,
            temporary,
        ))
    }

    /// Populate historical field bindings through the caller's retained work channel before admitting any data component.
    pub async fn new_from<'a>(
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
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let text =
            RetainedInvertedIndexBuilder::from_revisions_from(default, fields, control, input)
                .await?;
        Ok(Self::with_text(
            text,
            registry,
            control,
            value_limit,
            temporary,
        ))
    }

    fn with_text(
        text: RetainedInvertedIndexBuilder,
        registry: &MemoryCaptureRegistry,
        control: &StorageReadControl,
        value_limit: usize,
        temporary: &DiskANNTemporaryBudget,
    ) -> Self {
        Self {
            documents: DocumentInput::Ready(MemoryDocumentStoreBuilder::new(control)),
            text: DocumentInput::Ready(text),
            vectors: VectorInput::Ready(MemoryVectorIndexesBuilder::new(registry, control)),
            values: ValueInput::Ready(MemoryValueIndexesBuilder::new(registry, control)),
            control: control.clone(),
            value_limit,
            temporary: temporary.clone(),
        }
    }

    /// Consume one complete native data image. The surrounding Storage image reader owns address order and required metadata admission; native component decoders reject duplicate identities and invalid or trailing payload bytes.
    pub fn read(
        mut self,
        unit: &DecodedMemoryTableUnit,
        reader: &mut dyn Read,
    ) -> StorageBackendResult<Self> {
        loop {
            let (next, complete) = self.advance(
                unit,
                reader,
                ImageDecodeWindow {
                    bytes: 16384,
                    steps: 64,
                },
            )?;
            self = next;
            if complete {
                return Ok(self);
            }
        }
    }

    /// Advance the selected data component. Native grammars, physical preparation and named source registration retain this call's byte/poll bounds and require the same address until complete.
    pub fn advance(
        mut self,
        unit: &DecodedMemoryTableUnit,
        reader: &mut dyn Read,
        window: ImageDecodeWindow,
    ) -> StorageBackendResult<(Self, bool)> {
        self.control.check()?;
        if window.bytes == 0 || window.steps == 0 {
            return Err(invalid(
                "table input requires a nonzero byte and poll window",
            ));
        }
        if self.values.reading() {
            return self.advance_values(unit, reader, window);
        }
        if matches!(self.vectors, VectorInput::Reading { .. }) {
            return self.advance_vector(unit, reader, window);
        }
        if !matches!(self.text, DocumentInput::Ready(_))
            && !matches!(unit, DecodedMemoryTableUnit::Text(_))
        {
            return Err(invalid("text document input has not completed"));
        }
        if let DecodedMemoryTableUnit::Document(id) = unit {
            let (documents, complete) = self.documents.read(
                *id,
                reader,
                window,
                MemoryDocumentStoreBuilder::document_input,
            )?;
            self.documents = documents;
            self.control.check()?;
            return Ok((self, complete));
        }
        if !matches!(self.documents, DocumentInput::Ready(_)) {
            return Err(invalid("document input has not completed"));
        }
        match unit {
            DecodedMemoryTableUnit::Metadata(_) => {
                Err(invalid("table metadata is not a data component"))
            }
            DecodedMemoryTableUnit::Document(_) => unreachable!("document component handled above"),
            DecodedMemoryTableUnit::Text(id) => {
                let (text, complete) = self.text.read(
                    *id,
                    reader,
                    window,
                    RetainedInvertedIndexBuilder::document_input,
                )?;
                self.text = text;
                self.control.check()?;
                Ok((self, complete))
            }
            DecodedMemoryTableUnit::Vector(field) => {
                let VectorInput::Ready(vectors) = self.vectors else {
                    unreachable!("pending vector input was handled above")
                };
                self.vectors = VectorInput::Reading {
                    field: field.clone(),
                    input: vectors.image_input(field.clone(), &self.temporary)?,
                };
                self.advance_vector(unit, reader, window)
            }
            DecodedMemoryTableUnit::ValueIndex { .. } => self.advance_values(unit, reader, window),
        }
    }

    fn advance_vector(
        mut self,
        unit: &DecodedMemoryTableUnit,
        reader: &mut dyn Read,
        window: ImageDecodeWindow,
    ) -> StorageBackendResult<(Self, bool)> {
        let VectorInput::Reading {
            field: selected,
            mut input,
        } = self.vectors
        else {
            unreachable!("selected vector input")
        };
        if !matches!(unit, DecodedMemoryTableUnit::Vector(field) if field == &selected) {
            return Err(invalid("vector input changed its selected field"));
        }
        if let Some(vectors) = input.advance(reader, window)? {
            self.vectors = VectorInput::Ready(vectors);
            self.control.check()?;
            Ok((self, true))
        } else {
            self.vectors = VectorInput::Reading {
                field: selected,
                input,
            };
            Ok((self, false))
        }
    }

    fn advance_values(
        mut self,
        unit: &DecodedMemoryTableUnit,
        reader: &mut dyn Read,
        window: ImageDecodeWindow,
    ) -> StorageBackendResult<(Self, bool)> {
        let (values, complete) = self
            .values
            .advance(unit, reader, window, self.value_limit)?;
        self.values = values;
        self.control.check()?;
        Ok((self, complete))
    }

    pub fn finish(self) -> StorageBackendResult<MemoryTableData> {
        let value = self;
        value.control.check()?;
        let DocumentInput::Ready(documents) = value.documents else {
            return Err(invalid("document input has not completed"));
        };
        let DocumentInput::Ready(text) = value.text else {
            return Err(invalid("text document input has not completed"));
        };
        Ok(MemoryTableData {
            documents: documents.finish()?,
            text: text.finish_writable()?,
            vectors: match value.vectors {
                VectorInput::Ready(vectors) => vectors.finish()?,
                VectorInput::Reading { .. } => {
                    return Err(invalid("vector input has not completed"))
                }
            },
            values: value.values.finish()?,
        })
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
