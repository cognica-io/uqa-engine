//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored vector fields retain their original buffers while physical bindings advance independently.

use super::{invalid, PersistentVectorIndexesBuilder, VectorIndexes};
use crate::{
    read_control::StorageReadControl, PersistentStorageBackend, StorageBackendError,
    StorageBackendResult, StorageName, VectorFieldSchema,
};
use std::sync::Arc;
use uqa_core::{
    image_io::ImageDecoder,
    memory::{Budgeted, MemoryError, MemoryReservation},
};

struct Fields {
    values: std::vec::IntoIter<VectorFieldSchema>,
    memory: MemoryReservation,
    array_bytes: usize,
}

impl Fields {
    fn new(fields: Budgeted<Vec<VectorFieldSchema>>) -> StorageBackendResult<Self> {
        let array_bytes = fields
            .capacity()
            .checked_mul(size_of::<VectorFieldSchema>())
            .ok_or(MemoryError::SizeOverflow)?;
        if fields.reserved_bytes() < array_bytes {
            return Err(invalid(
                "vector field array has no complete input admission",
            ));
        }
        let (values, memory) = fields.into_parts();
        Ok(Self {
            values: values.into_iter(),
            memory,
            array_bytes,
        })
    }

    fn next(&mut self) -> StorageBackendResult<Option<(Budgeted<String>, u32)>> {
        let Some(field) = self.values.next() else {
            return Ok(None);
        };
        let bytes = field.field.capacity();
        if self.memory.bytes() - self.array_bytes < bytes {
            return Err(invalid("vector field has no complete input admission"));
        }
        Ok(Some((
            Budgeted::new(field.field, self.memory.split(bytes)),
            field.dimensions,
        )))
    }
}

impl PersistentVectorIndexesBuilder {
    /// Reconstruct the ordinary canonical field map in stored order. Every visit and physical-name fragment uses the original work channel. Repeated fields preserve the first map key and final physical handle, matching ordinary ordered-map insertion; acceleration binding remains with the later index catalog.
    pub fn catalog_input(
        backend: Arc<dyn PersistentStorageBackend>,
        table: StorageName,
        fields: Budgeted<Vec<VectorFieldSchema>>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImageDecoder<VectorIndexes, StorageBackendError>> {
        table.check_control(control)?;
        if !fields.budget().shares_allowance(control.memory()) {
            return Err(invalid("vector catalog requires its original allowance"));
        }
        let mut fields = Fields::new(fields)?;
        let retained = control.clone();
        Ok(ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            move |mut reader| async move {
                let mut builder = Self::new(&retained);
                loop {
                    reader.input().checkpoint().await?;
                    let Some((field, dimensions)) = fields.next()? else {
                        break;
                    };
                    let physical =
                        StorageName::from_admitted(reader.input().copy_text(&field).await?)?;
                    let index = backend.canonical_vector_index_with_control(
                        table.clone(),
                        physical,
                        dimensions,
                        &retained,
                    )?;
                    builder = builder.insert_index(field, index, false)?;
                }
                drop(fields);
                builder.finish()
            },
        )?)
    }
}

#[cfg(test)]
mod tests;
