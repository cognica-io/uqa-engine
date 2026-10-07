//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private journal records preserve exact tensor bits, including empty replacements and signed zeros.

use super::{IndexKind, OwnedVectorMutation};
use crate::{
    spill_map::{invalid, Record},
    StorageBackendResult,
};
use uqa_core::memory::MemoryError;

pub(super) fn retained_bytes(
    metadata: &[u8],
    vectors: Option<&[Vec<f32>]>,
) -> Result<usize, MemoryError> {
    let mut bytes = size_of::<OwnedVectorMutation>()
        .checked_add(metadata.len())
        .ok_or(MemoryError::SizeOverflow)?;
    if let Some(vectors) = vectors {
        bytes = bytes
            .checked_add(
                vectors
                    .len()
                    .checked_mul(size_of::<Vec<f32>>())
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .ok_or(MemoryError::SizeOverflow)?;
        for vector in vectors {
            bytes = bytes
                .checked_add(
                    vector
                        .len()
                        .checked_mul(size_of::<f32>())
                        .ok_or(MemoryError::SizeOverflow)?,
                )
                .ok_or(MemoryError::SizeOverflow)?;
        }
    }
    Ok(bytes)
}

impl Record for OwnedVectorMutation {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(retained_bytes(&self.metadata, self.vectors.as_deref())?)
    }
    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        let mut bytes = 18_usize
            .checked_add(self.metadata.len())
            .ok_or(MemoryError::SizeOverflow)?;
        if let Some(vectors) = &self.vectors {
            bytes = bytes.checked_add(8).ok_or(MemoryError::SizeOverflow)?;
            for vector in vectors {
                bytes = bytes
                    .checked_add(8)
                    .and_then(|bytes| bytes.checked_add(vector.len().checked_mul(4)?))
                    .ok_or(MemoryError::SizeOverflow)?;
            }
        }
        Ok(bytes)
    }
    fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.push(self.kind.fingerprint_tag());
        bytes.extend_from_slice(&(self.metadata.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&self.metadata);
        bytes.extend_from_slice(&self.document.to_le_bytes());
        bytes.push(u8::from(self.vectors.is_some()));
        if let Some(vectors) = &self.vectors {
            bytes.extend_from_slice(&(vectors.len() as u64).to_le_bytes());
            for vector in vectors {
                bytes.extend_from_slice(&(vector.len() as u64).to_le_bytes());
                for value in vector {
                    bytes.extend_from_slice(&value.to_bits().to_le_bytes());
                }
            }
        }
    }
    fn decode(mut bytes: &[u8]) -> StorageBackendResult<Self> {
        let kind = match take(&mut bytes, 1)?[0] {
            0 => IndexKind::IVFIndex,
            1 => IndexKind::HNSWIndex,
            _ => return Err(invalid("vector input kind")),
        };
        let length = number(&mut bytes)?;
        let metadata = take(&mut bytes, length)?.to_vec();
        let document = u64::from_le_bytes(take(&mut bytes, 8)?.try_into().unwrap());
        let vectors = match take(&mut bytes, 1)?[0] {
            0 => None,
            1 => {
                let count = number(&mut bytes)?;
                if count > bytes.len() / 8 {
                    return Err(invalid("vector input tensor count"));
                }
                let mut vectors = Vec::with_capacity(count);
                for _ in 0..count {
                    let dimensions = number(&mut bytes)?;
                    let length = dimensions.checked_mul(4).ok_or(MemoryError::SizeOverflow)?;
                    let raw = take(&mut bytes, length)?;
                    vectors.push(
                        raw.as_chunks::<4>()
                            .0
                            .iter()
                            .map(|bits| f32::from_bits(u32::from_le_bytes(*bits)))
                            .collect(),
                    );
                }
                Some(vectors)
            }
            _ => return Err(invalid("vector input presence")),
        };
        if !bytes.is_empty() {
            return Err(invalid("trailing vector input bytes"));
        }
        Ok(Self {
            kind,
            metadata,
            document,
            vectors,
        })
    }
}

fn take<'a>(bytes: &mut &'a [u8], length: usize) -> StorageBackendResult<&'a [u8]> {
    let (head, tail) = bytes
        .split_at_checked(length)
        .ok_or_else(|| invalid("truncated vector input"))?;
    *bytes = tail;
    Ok(head)
}
fn number(bytes: &mut &[u8]) -> StorageBackendResult<usize> {
    usize::try_from(u64::from_le_bytes(take(bytes, 8)?.try_into().unwrap()))
        .map_err(|_| invalid("vector input length"))
}
