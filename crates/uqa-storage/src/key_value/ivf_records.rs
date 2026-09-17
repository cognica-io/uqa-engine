//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key/Value IVF wire records; numerical preparation belongs to the common index owner.

use super::{
    codec::{decode_u64_value, decode_value, read_segment, read_u64},
    ivf_persistence::{metadata_from_snapshot, PersistedIVFMetadata, IVF_FORMAT_VERSION},
    TAG_IVF_ASSIGNMENT, TAG_IVF_CENTROID, TAG_IVF_METADATA, TAG_VECTOR,
};
use crate::{
    mvcc::{
        IVFRecordHeader, IVFRecordKey, IVFRecordLayout, IVFRecordValue, VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};
use uqa_core::{memory::BudgetedVec, DocId};

// These tombstone fences are outside canonical/index data, so drop/recreation cannot recycle them.
const GUARD: u8 = b'y';
pub struct KeyValueIVFRecords;

fn field_end(key: &[u8]) -> VersionResult<usize> {
    let mut offset = 1;
    for _ in 0..2 {
        let name = read_segment(key, &mut offset)?;
        std::str::from_utf8(name).map_err(|_| VersionError::InvalidEncoding("invalid IVF name"))?;
    }
    Ok(offset)
}
fn tail(key: &[u8], tag: u8, numbers: usize) -> VersionResult<(usize, [u64; 2])> {
    if key.first() != Some(&tag) {
        return Err(VersionError::InvalidEncoding("wrong IVF record family"));
    }
    let end = field_end(key)?;
    let mut offset = end;
    let mut output = [0; 2];
    for number in output.iter_mut().take(numbers) {
        *number = read_u64(key, &mut offset)?;
    }
    if offset != key.len() {
        return Err(VersionError::InvalidEncoding("invalid IVF key suffix"));
    }
    Ok((end, output))
}
fn usize_value(value: u64) -> VersionResult<usize> {
    usize::try_from(value)
        .map_err(|_| VersionError::InvalidEncoding("IVF counter exceeds addressable memory"))
}
fn ordinal(value: u64) -> VersionResult<u32> {
    u32::try_from(value).map_err(|_| VersionError::InvalidEncoding("invalid IVF vector ordinal"))
}
fn vector_bytes(value: &[u8], control: &StorageReadControl) -> VersionResult<BudgetedVec<f32>> {
    if !value.len().is_multiple_of(4) {
        return Err(VersionError::InvalidEncoding("invalid IVF vector payload"));
    }
    let mut output = BudgetedVec::new(control.memory());
    output.reserve(value.len() / 4)?;
    for bytes in value.chunks_exact(4) {
        control.cancellation().check()?;
        output.push(f32::from_le_bytes(
            bytes.try_into().expect("four-byte chunk"),
        ))?;
    }
    Ok(output)
}

impl IVFRecordLayout for KeyValueIVFRecords {
    fn metadata_key(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        control.cancellation().check()?;
        let Some(tag) = key.first().copied() else {
            return Ok(None);
        };
        let numbers = match tag {
            TAG_IVF_METADATA => 0,
            TAG_IVF_CENTROID => 1,
            TAG_IVF_ASSIGNMENT => 2,
            _ => return Ok(None),
        };
        let (end, _) = tail(key, tag, numbers)?;
        let mut output = BudgetedVec::new(control.memory());
        output.extend_from_slice(&key[..end])?;
        output[0] = TAG_IVF_METADATA;
        Ok(Some(output))
    }
    fn key(
        &self,
        metadata: &[u8],
        address: IVFRecordKey,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        control.cancellation().check()?;
        tail(metadata, TAG_IVF_METADATA, 0)?;
        let mut key = BudgetedVec::new(control.memory());
        key.extend_from_slice(metadata)?;
        match address {
            IVFRecordKey::Structure => {
                key[0] = GUARD;
                key.push(0)?;
            }
            IVFRecordKey::Document(document) => {
                key[0] = GUARD;
                key.push(1)?;
                key.extend_from_slice(&document.to_be_bytes())?;
            }
            IVFRecordKey::Vectors => key[0] = TAG_VECTOR,
            IVFRecordKey::Centroids => key[0] = TAG_IVF_CENTROID,
            IVFRecordKey::Assignments => key[0] = TAG_IVF_ASSIGNMENT,
            IVFRecordKey::Centroid(centroid) => {
                key[0] = TAG_IVF_CENTROID;
                key.extend_from_slice(&(centroid as u64).to_be_bytes())?;
            }
            IVFRecordKey::Assignment(document, ordinal) => {
                key[0] = TAG_IVF_ASSIGNMENT;
                key.extend_from_slice(&document.to_be_bytes())?;
                key.extend_from_slice(&u64::from(ordinal).to_be_bytes())?;
            }
        }
        Ok(key)
    }
    fn header(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<IVFRecordHeader> {
        control.cancellation().check()?;
        tail(key, TAG_IVF_METADATA, 0)?;
        let meta: PersistedIVFMetadata = decode_value(value)?;
        if meta.format_version != IVF_FORMAT_VERSION {
            return Err(VersionError::InvalidEncoding(
                "unsupported IVF record format",
            ));
        }
        let params = crate::IVFIndexParams {
            nlist: usize_value(meta.nlist)?,
            nprobe: usize_value(meta.nprobe)?,
            train_threshold: usize_value(meta.train_threshold)?,
        }
        .validate()?;
        Ok(IVFRecordHeader {
            dimensions: meta.dimensions,
            params,
            state: meta.state.into(),
            trained_size: usize_value(meta.trained_size)?,
            deletes_since_train: usize_value(meta.deletes_since_train)?,
            vector_count: usize_value(meta.vector_count)?,
            revision: meta.revision,
        })
    }
    fn vector_id(&self, key: &[u8]) -> VersionResult<(DocId, u32)> {
        let (_, [document, order]) = tail(key, TAG_VECTOR, 2)?;
        Ok((document, ordinal(order)?))
    }
    fn vector(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32, BudgetedVec<f32>)> {
        let (document, order) = self.vector_id(key)?;
        Ok((document, order, vector_bytes(value, control)?))
    }
    fn centroid(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(usize, BudgetedVec<f32>)> {
        let (_, [centroid, _]) = tail(key, TAG_IVF_CENTROID, 1)?;
        Ok((usize_value(centroid)?, vector_bytes(value, control)?))
    }
    fn assignment(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32, usize)> {
        control.cancellation().check()?;
        let (_, [document, order]) = tail(key, TAG_IVF_ASSIGNMENT, 2)?;
        Ok((
            document,
            ordinal(order)?,
            usize_value(decode_u64_value(value)?)?,
        ))
    }
    fn encode(
        &self,
        key: &[u8],
        template: &[u8],
        value: IVFRecordValue<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        control.cancellation().check()?;
        let mut output = BudgetedVec::new(control.memory());
        match value {
            IVFRecordValue::Header { snapshot, revision } => {
                let header = self.header(key, template, control)?;
                let meta =
                    metadata_from_snapshot(header.dimensions, header.params, snapshot, revision)?;
                let mut writer = JsonWriter {
                    output,
                    control,
                    failure: None,
                };
                let encoded = serde_json::to_writer(&mut writer, &meta);
                if let Some(error) = writer.failure {
                    return Err(error);
                }
                encoded.map_err(crate::StorageBackendError::from)?;
                return Ok(writer.output);
            }
            IVFRecordValue::Centroid(vector) => {
                tail(key, TAG_IVF_CENTROID, 1)?;
                for value in vector {
                    control.cancellation().check()?;
                    output.extend_from_slice(&value.to_le_bytes())?;
                }
            }
            IVFRecordValue::Assignment(centroid) => {
                tail(key, TAG_IVF_ASSIGNMENT, 2)?;
                output.extend_from_slice(&(centroid as u64).to_be_bytes())?;
            }
        }
        Ok(output)
    }
}

struct JsonWriter<'a> {
    output: BudgetedVec<u8>,
    control: &'a StorageReadControl,
    failure: Option<VersionError>,
}
impl std::io::Write for JsonWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let result = self
            .control
            .cancellation()
            .check()
            .map_err(VersionError::from)
            .and_then(|()| {
                self.output
                    .extend_from_slice(bytes)
                    .map_err(VersionError::from)
            });
        if let Err(error) = result {
            self.failure = Some(error);
            return Err(std::io::Error::other("IVF encoding interrupted"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
