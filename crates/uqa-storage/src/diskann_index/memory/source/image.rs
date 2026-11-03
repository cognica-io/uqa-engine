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
    image_io::{ImageInput, ImageOutput},
    vector_index::image::{floats, invalid, read_floats},
    StorageBackendResult,
};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError};

impl Canonical {
    pub(in crate::diskann_index::memory) fn write_image(
        &self,
        output: &mut ImageOutput<'_>,
        generation: DiskANNGeneration,
        physical: &DiskANNMemorySource,
    ) -> StorageBackendResult<()> {
        self.validate_image(physical, generation, output.control)?;
        match self.revision {
            Some(revision) => {
                output.bytes(&[1])?;
                output.bytes(&DiskANNChangeIdentity::new(0, revision).encode())?;
            }
            None => output.bytes(&[0])?,
        }
        output.bytes(&self.counts.current_vectors().to_le_bytes())?;
        output.bytes(&self.counts.changed_vectors().to_le_bytes())?;
        output.count(self.documents.len())?;
        for (&document, tensor) in &self.documents {
            output.bytes(&document.to_le_bytes())?;
            output.bytes(&tensor.origin.encode())?;
            for vector in &*tensor.values {
                floats(output, vector)?;
            }
        }
        output.count(self.changes.len())?;
        for (&document, &version) in &self.changes {
            output.bytes(&DiskANNChangeIdentity::new(document, version).encode())?;
        }
        output.control.check()
    }

    pub(in crate::diskann_index::memory) fn read_image(
        input: &mut ImageInput<'_>,
        dimensions: u32,
        generation: DiskANNGeneration,
        physical: &DiskANNMemorySource,
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
        Ok(source)
    }

    fn validate_image(
        &self,
        physical: &DiskANNMemorySource,
        generation: DiskANNGeneration,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()?;
        if self.covered_generation != Some(generation) {
            return Err(invalid());
        }
        let mut current = 0_u64;
        let mut changed = 0_u64;
        let mut revision_seen = self.revision.is_none() && self.documents.is_empty();
        for (&document, tensor) in &self.documents {
            control.check()?;
            if tensor.origin.dimensions() != self.dimensions
                || tensor.origin.count() != tensor.values.len() as u64
            {
                return Err(invalid());
            }
            current = current
                .checked_add(tensor.origin.count())
                .ok_or(MemoryError::SizeOverflow)?;
            if let Some(version) = self.changes.get(&document) {
                if *version != tensor.origin.version() {
                    return Err(invalid());
                }
                changed = changed
                    .checked_add(tensor.origin.count())
                    .ok_or(MemoryError::SizeOverflow)?;
            }
            revision_seen |= Some(tensor.origin.version()) == self.revision;
        }
        if !revision_seen || self.counts != DiskANNCanonicalCounts::new(current, changed)? {
            return Err(invalid());
        }
        for (document, _) in &self.changes {
            control.check()?;
            if self.documents.get(document).is_none() {
                return Err(invalid());
            }
        }
        let mut documents = self.documents.iter().peekable();
        physical.visit_image_origins(control, &mut |base| {
            while documents
                .peek()
                .is_some_and(|(document, _)| **document < base.document())
            {
                let (document, _) = documents.next().expect("peeked document");
                if self.changes.get(document).is_none() {
                    return Err(invalid());
                }
            }
            let (document, tensor) = documents.next().ok_or_else(invalid)?;
            let uncovered = tensor.origin != base.origin();
            if *document != base.document() || uncovered != self.changes.get(document).is_some() {
                return Err(invalid());
            }
            Ok(())
        })?;
        for (document, _) in documents {
            control.check()?;
            if self.changes.get(document).is_none() {
                return Err(invalid());
            }
        }
        self.control.check()?;
        control.check()
    }
}
