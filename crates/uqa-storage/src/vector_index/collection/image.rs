//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native vector registration adopts original decoded names and admits every map node.

use super::{native::NativeVectorSources, Contents, NativeIndexes, VectorIndex, VectorIndexes};
use crate::{
    memory_capture::MemoryCaptureRegistry, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use uqa_core::image_io::{AsyncImageInput, ImageDecoder};
use uqa_core::memory::{Budgeted, MemoryBudget};

/// An unpublished writable registration set. Each vector already owns its decoded allocations; names and native membership use the supplied allowance. A failed addition consumes the whole candidate. Completion keeps every original reservation while subsequent mutations use the ordinary native allowance independently of import limits and cancellation.
pub struct MemoryVectorIndexesBuilder {
    indexes: NativeIndexes,
    capture: NativeVectorSources,
    control: StorageReadControl,
}

impl MemoryVectorIndexesBuilder {
    pub fn new(registry: &MemoryCaptureRegistry, control: &StorageReadControl) -> Self {
        Self {
            indexes: NativeIndexes::new(),
            capture: NativeVectorSources::new(registry, control.memory()),
            control: control.clone(),
        }
    }

    /// Adopt one unique original field and its writable evaluated index. Registering a source does not build or retrain the physical index.
    pub fn add_index(
        self,
        field: Budgeted<String>,
        index: Box<dyn VectorIndex>,
    ) -> StorageBackendResult<Self> {
        let index = self.prepare_index(&field, index)?;
        let capture = self.capture.with_index(&field, index.as_ref())?;
        self.publish_index(field, index, capture)
    }

    /// Retain field admission, native image decoding and source registration through one input channel. The field owner remains immutable until the selected component completes.
    pub fn image_input(
        self,
        field: impl AsRef<str> + Send + Sync + 'static,
        temporary: &crate::diskann_index::build::DiskANNTemporaryBudget,
    ) -> StorageBackendResult<ImageDecoder<Self, StorageBackendError>> {
        let control = self.control.clone();
        let scope = control.clone();
        let temporary = temporary.clone();
        Ok(ImageDecoder::new(
            scope.memory(),
            scope.cancellation(),
            move |mut reader| async move {
                let mut source = reader.input();
                let field = source.copy_text(field.as_ref()).await?;
                let index = {
                    let mut input = crate::vector_index::image::Input {
                        source: &mut source,
                        control: &control,
                    };
                    crate::vector_index::image::read_vector_from(&mut input, Some(&temporary))
                        .await?
                };
                self.add_index_from(field, index, &mut source).await
            },
        )?)
    }

    async fn add_index_from(
        self,
        field: Budgeted<String>,
        index: Box<dyn VectorIndex>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        input.checkpoint().await?;
        let index = self.prepare_index(&field, index)?;
        let capture = self
            .capture
            .with_index_from(&field, index.as_ref(), input)
            .await?;
        self.publish_index(field, index, capture)
    }

    fn prepare_index(
        &self,
        field: &Budgeted<String>,
        index: Box<dyn VectorIndex>,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.control.check()?;
        if !field.budget().shares_allowance(self.control.memory())
            || field.reserved_bytes() < field.capacity()
        {
            return Err(invalid("vector field has no complete input admission"));
        }
        if self.indexes.contains_key(field.as_str()) {
            return Err(invalid("vector image repeats a field registration"));
        }
        let index =
            index.into_memory_capture_with_control(&self.capture.registry, &self.control)?;
        self.control.check()?;
        Ok(index)
    }

    fn publish_index(
        mut self,
        field: Budgeted<String>,
        index: Box<dyn VectorIndex>,
        capture: NativeVectorSources,
    ) -> StorageBackendResult<Self> {
        self.indexes.insert_admitted_key(field, index.into())?;
        self.capture = capture;
        self.control.check()?;
        Ok(self)
    }

    pub fn finish(self) -> StorageBackendResult<VectorIndexes> {
        self.control.check()?;
        Ok(VectorIndexes(Contents::Live {
            indexes: self.indexes,
            capture: Some(
                self.capture
                    .into_mutation_budget(&MemoryBudget::new(usize::MAX)),
            ),
        }))
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
