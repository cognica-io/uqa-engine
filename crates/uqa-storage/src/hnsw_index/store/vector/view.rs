//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected resident and spilled vectors expose fixed coordinate windows without full decoding.

use crate::{
    hnsw_index::types::HNSWVector,
    read_control::StorageReadControl,
    spill_map::{invalid, Map, RecordBytes, RecordRead},
    StorageBackendResult,
};
use uqa_core::memory::MemoryError;

#[cfg(test)]
mod tests;

enum Values<'a> {
    Resident(&'a [f32]),
    Spilled(RecordBytes<'a>),
}

pub(in crate::hnsw_index) struct VectorRead<'a> {
    values: Values<'a>,
    length: usize,
    norm: f32,
}

impl<'a> VectorRead<'a> {
    pub(in crate::hnsw_index) fn open(
        map: &'a Map<HNSWVector>,
        key: u128,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Self>> {
        let Some(record) = map.read_record(key, control)? else {
            return Ok(None);
        };
        let view = match record {
            RecordRead::Resident(vector) => Self {
                values: Values::Resident(&vector.values),
                length: vector.values.len(),
                norm: vector.norm,
            },
            RecordRead::Spilled(record) => {
                let header = record.array::<12>(0, control)?;
                let norm = f32::from_bits(u32::from_le_bytes(
                    header[..4].try_into().expect("fixed norm"),
                ));
                let length = usize::try_from(u64::from_le_bytes(
                    header[4..].try_into().expect("fixed count"),
                ))
                .map_err(|_| MemoryError::SizeOverflow)?;
                let bytes = record.len() - 12;
                if length > bytes / 4 {
                    return Err(invalid("node array length"));
                }
                if length * 4 != bytes {
                    return Err(invalid("trailing vector bytes"));
                }
                Self {
                    values: Values::Spilled(record),
                    length,
                    norm,
                }
            }
        };
        control.check()?;
        Ok(Some(view))
    }

    pub(in crate::hnsw_index) fn len(&self) -> usize {
        self.length
    }

    pub(in crate::hnsw_index) fn norm(&self) -> f32 {
        self.norm
    }

    /// Copy at most 1,024 actual coordinates. Only the caller's fixed output window is materialized.
    pub(in crate::hnsw_index) fn read_values(
        &self,
        offset: usize,
        output: &mut [f32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<usize> {
        control.check()?;
        let remaining = self
            .length
            .checked_sub(offset)
            .ok_or_else(|| invalid("vector read offset"))?;
        let count = remaining.min(output.len()).min(1024);
        match &self.values {
            Values::Resident(values) => {
                output[..count].copy_from_slice(&values[offset..offset + count]);
            }
            Values::Spilled(record) => {
                let mut bytes = [0; 4096];
                let start = offset
                    .checked_mul(4)
                    .and_then(|n| n.checked_add(12))
                    .ok_or(MemoryError::SizeOverflow)?;
                if record.read_at(start, &mut bytes[..count * 4], control)? != count * 4 {
                    return Err(invalid("truncated vector coordinates"));
                }
                for (value, bytes) in output[..count]
                    .iter_mut()
                    .zip(bytes[..count * 4].as_chunks::<4>().0)
                {
                    *value = f32::from_bits(u32::from_le_bytes(*bytes));
                }
            }
        }
        control.check()?;
        Ok(count)
    }
}
