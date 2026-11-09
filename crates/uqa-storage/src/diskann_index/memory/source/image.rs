//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical images preserve exact tensor origins and uncovered changes without invoking mutation.

use super::{Canonical, Tensor};
use crate::{
    diskann_index::{
        format::{
            DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNGeneration,
            CANONICAL_ORIGIN_BYTES, CHANGE_IDENTITY_BYTES,
        },
        pages::DiskANNMemorySource,
        DiskANNCanonicalCounts,
    },
    image_io::ImageInput,
    vector_index::image::{invalid, read_floats},
    StorageBackendResult,
};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError};

mod stream;
#[cfg(test)]
mod tests;
mod validation;
pub(in crate::diskann_index::memory) use stream::CanonicalImageCursor;
pub(in crate::diskann_index::memory) use validation::CanonicalImageValidation;

impl Canonical {
    pub(in crate::diskann_index::memory) fn read_image(
        input: &mut ImageInput<'_>,
        dimensions: u32,
        generation: DiskANNGeneration,
        physical: &DiskANNMemorySource,
        owner: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut source = Self::empty(dimensions, input.control);
        source.covered_generation = Some(generation);
        source.revision = match input.array::<1>()?[0] {
            0 => None,
            1 => {
                let revision =
                    DiskANNChangeIdentity::decode(&input.array::<CHANGE_IDENTITY_BYTES>()?)?;
                if revision.document() != 0 {
                    return Err(invalid());
                }
                Some(revision.version())
            }
            _ => return Err(invalid()),
        };
        source.counts = DiskANNCanonicalCounts::new(
            u64::from_le_bytes(input.array()?),
            u64::from_le_bytes(input.array()?),
        )?;
        let count = input.count()?;
        let mut previous = None;
        for _ in 0..count {
            let document = u64::from_le_bytes(input.array()?);
            if previous.is_some_and(|previous| previous >= document) {
                return Err(invalid());
            }
            let origin = DiskANNCanonicalOrigin::decode(
                &input.array::<CANONICAL_ORIGIN_BYTES>()?,
                dimensions,
            )?;
            let mut values = BudgetedVec::new(input.control.memory());
            let mut memory = input.control.memory().empty_reservation();
            let count = usize::try_from(origin.count()).map_err(|_| MemoryError::SizeOverflow)?;
            values.reserve(count)?;
            for _ in 0..count {
                let (value, lease) = read_floats(input, dimensions)?.into_parts();
                memory.absorb(lease);
                values.push(value)?;
            }
            let (values, lease) = values.into_parts();
            memory.absorb(lease);
            source.documents.try_insert(
                document,
                Tensor {
                    origin,
                    values: Budgeted::new(values, memory),
                },
            )?;
            previous = Some(document);
        }
        let count = input.count()?;
        let mut previous = None;
        for _ in 0..count {
            let change = DiskANNChangeIdentity::decode(&input.array::<CHANGE_IDENTITY_BYTES>()?)?;
            if previous.is_some_and(|previous| previous >= change.document()) {
                return Err(invalid());
            }
            source
                .changes
                .try_insert(change.document(), change.version())?;
            previous = Some(change.document());
        }
        source.validate_image(physical, generation, input.control)?;
        source.documents = source.documents.into_mutation_budget(owner.memory());
        source.changes = source.changes.into_mutation_budget(owner.memory());
        source.control = owner.clone();
        Ok(source)
    }

    fn validate_image(
        &self,
        physical: &DiskANNMemorySource,
        generation: DiskANNGeneration,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<()> {
        let mut validation = CanonicalImageValidation::new(self, physical, generation)?;
        while !validation.advance(128, control)?.ready {}
        Ok(())
    }
}
