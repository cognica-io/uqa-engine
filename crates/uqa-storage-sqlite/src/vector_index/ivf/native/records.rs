//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native IVF row codecs; common storage owns training, ordered inputs and generation merging.

use super::super::metadata::{parse_state, positive_i64_to_usize, state_to_str};
use crate::mvcc::native::{
    decode_record, encode_row, NativeRecordFamily as Family, NativeRecordIdentity as Identity,
    NativeRecordOwner,
};
use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::{
    mvcc::{
        IVFRecordHeader, IVFRecordKey as Key, IVFRecordLayout, IVFRecordValue as Value,
        VersionError, VersionResult,
    },
    read_control::StorageReadControl,
    IVFIndexParams, StorageBackendError,
};

pub(crate) struct NativeIVFRecords;
use crate::vector_index::native::records::{
    integer, invalid, ordinal, signed, size, unsigned, vector, Address,
};

impl IVFRecordLayout for NativeIVFRecords {
    fn metadata_key(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        let identity = Identity::decode(key)?;
        if !matches!(
            identity.family(),
            Family::IVFIndexes | Family::IVFCentroids | Family::IVFAssignments
        ) {
            return Ok(None);
        }
        Ok(Some(Address::decode(key, control)?.key(
            Family::IVFIndexes,
            &[],
            control,
        )?))
    }
    fn key(
        &self,
        metadata: &[u8],
        key: Key,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let address = Address::decode(metadata, control)?;
        if address.identity.family() != Family::IVFIndexes {
            return Err(invalid());
        }
        match key {
            Key::Structure => address.key(Family::VectorGuards, &[-1], control),
            Key::Document(document) => {
                address.key(Family::VectorGuards, &[signed(document)?], control)
            }
            Key::Vectors => address.key(Family::Vectors, &[], control),
            Key::Centroids => address.key(Family::IVFCentroids, &[], control),
            Key::Assignments => address.key(Family::IVFAssignments, &[], control),
            Key::Centroid(id) => address.key(Family::IVFCentroids, &[signed(id as u64)?], control),
            Key::Assignment(document, order) => address.key(
                Family::IVFAssignments,
                &[signed(document)?, i64::from(order)],
                control,
            ),
        }
    }
    fn header(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<IVFRecordHeader> {
        let (identity, row) = decode_record(key, value, control)?;
        if identity.family() != Family::IVFIndexes {
            return Err(invalid());
        }
        let positive = |name, value| {
            positive_i64_to_usize(name, value)
                .map_err(StorageBackendError::from)
                .map_err(VersionError::from)
        };
        Ok(IVFRecordHeader {
            dimensions: u32::try_from(integer(row[2])?).map_err(|_| invalid())?,
            params: IVFIndexParams {
                nlist: positive("nlist", integer(row[3])?)?,
                nprobe: positive("nprobe", integer(row[4])?)?,
                train_threshold: positive("train_threshold", integer(row[5])?)?,
            }
            .validate()?,
            state: parse_state(row[6].as_str().map_err(|_| invalid())?)
                .map_err(StorageBackendError::from)?,
            trained_size: size(integer(row[7])?)?,
            deletes_since_train: size(integer(row[8])?)?,
            vector_count: size(integer(row[9])?)?,
            revision: None,
        })
    }
    fn vector_id(&self, key: &[u8], control: &StorageReadControl) -> VersionResult<(DocId, u32)> {
        crate::vector_index::native::records::vector_id(key, control)
    }

    fn vector(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32, BudgetedVec<f32>)> {
        crate::vector_index::native::records::vector_record(key, value, control)
    }

    fn centroid(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(usize, BudgetedVec<f32>)> {
        let (identity, row) = decode_record(key, value, control)?;
        if identity.family() != Family::IVFCentroids {
            return Err(invalid());
        }
        Ok((size(integer(row[2])?)?, vector(row[3], control)?))
    }
    fn assignment(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32, usize)> {
        let (identity, row) = decode_record(key, value, control)?;
        if identity.family() != Family::IVFAssignments {
            return Err(invalid());
        }
        Ok((
            unsigned(integer(row[2])?)?,
            ordinal(integer(row[3])?)?,
            size(integer(row[4])?)?,
        ))
    }
    fn encode(
        &self,
        key: &[u8],
        template: &[u8],
        value: Value<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let metadata = self.metadata_key(key, control)?.ok_or_else(invalid)?;
        let (_, mut row) = decode_record(&metadata, template, control)?;
        let address = Address::decode(key, control)?;
        match value {
            Value::Header { snapshot, revision } => {
                if address.identity.family() != Family::IVFIndexes || revision.is_some() {
                    return Err(invalid());
                }
                row[6] = ValueRef::Text(state_to_str(snapshot.state).as_bytes());
                row[7] = ValueRef::Integer(signed(snapshot.trained_size as u64)?);
                row[8] = ValueRef::Integer(signed(snapshot.deletes_since_train as u64)?);
                row[9] = ValueRef::Integer(signed(snapshot.vector_count as u64)?);
                encode_row(&row, control)
            }
            Value::Centroid(vector) => {
                if address.identity.family() != Family::IVFCentroids || address.numbers[0] < 0 {
                    return Err(invalid());
                }
                let mut bytes = BudgetedVec::new(control.memory());
                for value in vector {
                    control.cancellation().check()?;
                    bytes.extend_from_slice(&value.to_le_bytes())?;
                }
                encode_row(
                    &[
                        row[0],
                        row[1],
                        ValueRef::Integer(address.numbers[0]),
                        ValueRef::Blob(&bytes),
                    ],
                    control,
                )
            }
            Value::Assignment(centroid) => {
                if address.identity.family() != Family::IVFAssignments {
                    return Err(invalid());
                }
                unsigned(address.numbers[0])?;
                ordinal(address.numbers[1])?;
                encode_row(
                    &[
                        row[0],
                        row[1],
                        ValueRef::Integer(address.numbers[0]),
                        ValueRef::Integer(address.numbers[1]),
                        ValueRef::Integer(signed(centroid as u64)?),
                    ],
                    control,
                )
            }
        }
    }
}

pub(crate) fn metadata_key(
    owner: NativeRecordOwner,
    field: &str,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    Identity::new(Family::IVFIndexes, owner)?
        .encode_key(&[ValueRef::Text(field.as_bytes())], control)
}

#[cfg(test)]
mod tests;
