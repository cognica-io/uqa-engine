//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document-scoped canonical reads retain origins and all tensor ordinals on one boundary.

use super::{invalid, Record, BYTES};
use crate::diskann_index::format::DiskANNVectorVersion;
use crate::key_value::KeyValueRead;
use crate::read_control::StorageReadControl;
use crate::StorageBackendResult;
use std::sync::Arc;
use uqa_core::memory::BudgetedVec;
use uqa_core::DocId;

pub type DiskANNCanonicalVectorVisitor<'a> =
    dyn FnMut(u32, DiskANNVectorVersion, &[f32]) -> StorageBackendResult<()> + 'a;

/// A fixed canonical view. Visitors borrow one decoded vector at a time and must not reenter the source from inside a callback. Any failure invalidates the caller's partial result.
pub struct RetainedDiskANNCanonical {
    read: Arc<dyn KeyValueRead + Send + Sync>,
    vectors: BudgetedVec<u8>,
    origins: BudgetedVec<u8>,
    dimensions: u32,
    control: StorageReadControl,
}

impl RetainedDiskANNCanonical {
    pub(super) fn new(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        vectors: &[u8],
        origins: &[u8],
        dimensions: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            read,
            vectors: append(vectors, &[], control)?,
            origins: append(origins, &[], control)?,
            dimensions,
            control: control.clone(),
        })
    }

    /// Return a document's origin only after checking that its complete contiguous canonical ordinal set agrees with the stored replacement count. A zero-count record is an explicit empty replacement.
    pub fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.record(document, control)
            .map(|record| record.map(|record| record.version))
    }

    /// Stream the complete visible tensor, preserving raw coordinate bits. No navigation estimate, candidate deduplication or score conversion occurs here.
    pub fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        let Some(record) = self.record(document, control)? else {
            return Ok(None);
        };
        if record.count == 0 {
            return Ok(Some(record.version));
        }
        let document_key = append(&self.vectors, &document.to_be_bytes(), control)?;
        let dimensions = usize::try_from(self.dimensions)
            .map_err(|_| invalid("canonical dimensions exceed platform size"))?;
        let bytes = dimensions
            .checked_mul(4)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let mut vector = BudgetedVec::new(control.memory());
        vector.reserve(dimensions)?;
        for ordinal in 0..record.count {
            self.control.check()?;
            control.check()?;
            let key = append(&document_key, &ordinal.to_be_bytes(), control)?;
            self.read
                .visit_value_bounded(&key, bytes, control, &mut |value| {
                    let value = value.ok_or_else(|| invalid("missing canonical ordinal"))?;
                    if value.len() != bytes {
                        return Err(invalid("canonical vector width mismatch"));
                    }
                    vector.clear();
                    for (coordinate, chunk) in value.chunks_exact(4).enumerate() {
                        if coordinate.is_multiple_of(1024) {
                            self.control.check()?;
                            control.check()?;
                        }
                        vector.push(f32::from_le_bytes(chunk.try_into().expect("fixed chunks")))?;
                    }
                    crate::vector_index::validate_vector_values_controlled(
                        self.dimensions,
                        &vector,
                        Some(control),
                    )?;
                    visit(
                        u32::try_from(ordinal)
                            .map_err(|_| invalid("canonical ordinal overflow"))?,
                        record.version,
                        &vector,
                    )
                })?;
        }
        self.control.check()?;
        control.check()?;
        Ok(Some(record.version))
    }

    fn record(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Record>> {
        self.control.check()?;
        control.check()?;
        let key = append(&self.origins, &document.to_be_bytes(), control)?;
        let mut selected = None;
        self.read
            .visit_value_bounded(&key, BYTES, control, &mut |value| {
                selected = value
                    .map(|value| Record::decode(value, self.dimensions))
                    .transpose()?;
                Ok(())
            })?;
        let prefix = append(&self.vectors, &document.to_be_bytes(), control)?;
        let expected = selected.map_or(0, |record| record.count);
        let mut count = 0_u64;
        self.read
            .visit_keys_after(&prefix, None, usize::MAX, control, &mut |key| {
                self.control.check()?;
                control.check()?;
                if count >= expected
                    || key.strip_prefix(&*prefix) != Some(count.to_be_bytes().as_slice())
                {
                    return Err(invalid("canonical origins or ordinal coverage mismatch"));
                }
                count += 1;
                Ok(())
            })?;
        if count != expected {
            return Err(invalid("canonical replacement count mismatch"));
        }
        self.control.check()?;
        control.check()?;
        Ok(selected)
    }
}

fn append(
    prefix: &[u8],
    suffix: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    let mut key = BudgetedVec::new(control.memory());
    key.reserve(
        prefix
            .len()
            .checked_add(suffix.len())
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
    )?;
    key.extend_from_slice(prefix)?;
    key.extend_from_slice(suffix)?;
    Ok(key)
}
