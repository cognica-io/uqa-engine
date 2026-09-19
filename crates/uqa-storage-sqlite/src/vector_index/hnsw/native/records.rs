//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native HNSW headers, nodes and separate edges implement the common vector merge contract.

use super::super::encoding::{checked_hnsw_level, decode_meta};
use crate::mvcc::native::{
    decode_record, encode_row, NativeRecordFamily as Family, NativeRecordIdentity as Identity,
    NativeRecordOwner,
};
use crate::vector_index::native::records::{
    integer, invalid, ordinal, signed, unsigned, vector, Address,
};
use rusqlite::types::ValueRef;
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};
use uqa_storage::{
    hnsw_index::HNSWNodeSnapshot,
    mvcc::{
        HNSWRecordHeader, HNSWRecordKey as Key, HNSWRecordLayout, HNSWRecordValue as Value,
        VersionError, VersionResult,
    },
    read_control::StorageReadControl,
    StorageBackendError,
};

pub(crate) struct NativeHNSWRecords;

#[cfg(test)]
mod tests;

fn level(value: i64) -> VersionResult<usize> {
    checked_hnsw_level("layer", value)
        .map_err(StorageBackendError::from)
        .map_err(VersionError::from)
}

impl HNSWRecordLayout for NativeHNSWRecords {
    fn metadata_key(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        let identity = Identity::decode(key)?;
        if !matches!(
            identity.family(),
            Family::HNSWIndexes | Family::HNSWNodes | Family::HNSWEdges
        ) {
            return Ok(None);
        }
        Ok(Some(Address::decode(key, control)?.key(
            Family::HNSWIndexes,
            &[],
            control,
        )?))
    }
    fn key(
        &self,
        metadata: &[u8],
        address: Key,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let scope = Address::decode(metadata, control)?;
        if scope.identity.family() != Family::HNSWIndexes {
            return Err(invalid());
        }
        match address {
            Key::Structure => scope.key(Family::VectorGuards, &[-1], control),
            Key::Document(document) => {
                scope.key(Family::VectorGuards, &[signed(document)?], control)
            }
            Key::Vectors => scope.key(Family::Vectors, &[], control),
            Key::Nodes => scope.key(Family::HNSWNodes, &[], control),
            Key::Node(node) => scope.key(Family::HNSWNodes, &[signed(node)?], control),
            Key::Edge {
                source,
                layer,
                target,
            } => {
                let layer = signed(layer as u64)?;
                self::level(layer)?;
                scope.key(
                    Family::HNSWEdges,
                    &[signed(source)?, layer, signed(target)?],
                    control,
                )
            }
        }
    }
    fn edges_prefix(
        &self,
        metadata: &[u8],
        source: Option<u64>,
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        let address = Address::decode(metadata, control)?;
        if address.identity.family() != Family::HNSWIndexes {
            return Err(invalid());
        }
        let source = source.map(signed).transpose()?.map(|id| [id]);
        Ok(Some(address.key(
            Family::HNSWEdges,
            source.as_ref().map_or(&[], |values| values.as_slice()),
            control,
        )?))
    }
    fn header(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<HNSWRecordHeader> {
        let (identity, row) = decode_record(key, value, control)?;
        if identity.family() != Family::HNSWIndexes {
            return Err(invalid());
        }
        let (dimensions, params, meta, revision) = decode_meta((
            integer(row[2])?,
            integer(row[3])?,
            integer(row[4])?,
            integer(row[5])?,
            integer(row[6])?,
            row[7].as_str().map_err(|_| invalid())?,
            if row[8] == ValueRef::Null {
                None
            } else {
                Some(integer(row[8])?)
            },
            integer(row[9])?,
            integer(row[10])?,
            integer(row[11])?,
            integer(row[12])?,
            integer(row[13])?,
            integer(row[14])?,
        ))
        .map_err(StorageBackendError::from)?;
        Ok(HNSWRecordHeader {
            dimensions,
            params,
            meta,
            revision: Some(revision),
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

    fn node(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Budgeted<HNSWNodeSnapshot>> {
        control.cancellation().check()?;
        let mut memory = control.memory().reserve(size_of::<HNSWNodeSnapshot>())?;
        let (identity, row) = decode_record(key, value, control)?;
        if identity.family() != Family::HNSWNodes {
            return Err(invalid());
        }
        let level = level(integer(row[5])?)?;
        let deleted = match integer(row[6])? {
            0 => false,
            1 => true,
            _ => return Err(invalid()),
        };
        let node_id = unsigned(integer(row[2])?)?;
        let doc_id = unsigned(integer(row[3])?)?;
        let vector_ordinal = ordinal(integer(row[4])?)?;
        let mut layers = BudgetedVec::new(control.memory());
        for _ in 0..=level {
            layers.push(Vec::new())?;
        }
        let (raw_vector, retained) = vector(row[7], control)?.into_parts();
        memory.absorb(retained);
        let (neighbors, retained) = layers.into_parts();
        memory.absorb(retained);
        Ok(Budgeted::new(
            HNSWNodeSnapshot {
                node_id,
                doc_id,
                vector_ordinal,
                raw_vector,
                level,
                deleted,
                neighbors,
            },
            memory,
        ))
    }
    fn edge(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(u64, usize, u64)> {
        let (identity, row) = decode_record(key, value, control)?;
        if identity.family() != Family::HNSWEdges {
            return Err(invalid());
        }
        Ok((
            unsigned(integer(row[2])?)?,
            level(integer(row[3])?)?,
            unsigned(integer(row[4])?)?,
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
        self.header(&metadata, template, control)?;
        let (_, mut row) = decode_record(&metadata, template, control)?;
        let address = Address::decode(key, control)?;
        let int = ValueRef::Integer;
        match value {
            Value::Header { meta, revision } => {
                if address.identity.family() != Family::HNSWIndexes {
                    return Err(invalid());
                }
                row[8] = meta
                    .entry_point
                    .map(|id| signed(id).map(int))
                    .transpose()?
                    .unwrap_or(ValueRef::Null);
                row[9] = int(signed(meta.max_level as u64)?);
                level(integer(row[9])?)?;
                row[10] = int(signed(meta.next_node_id)?);
                row[11] = int(signed(meta.live_count as u64)?);
                row[12] = int(signed(meta.deleted_count as u64)?);
                row[13] = int(signed(revision.ok_or_else(invalid)?)?);
                encode_row(&row, control)
            }
            Value::Node(node) => {
                if address.identity.family() != Family::HNSWNodes
                    || unsigned(address.numbers[0])? != node.node_id
                {
                    return Err(invalid());
                }
                level(signed(node.level as u64)?)?;
                let mut bytes = BudgetedVec::new(control.memory());
                for value in &node.raw_vector {
                    control.cancellation().check()?;
                    bytes.extend_from_slice(&value.to_le_bytes())?;
                }
                encode_row(
                    &[
                        row[0],
                        row[1],
                        int(signed(node.node_id)?),
                        int(signed(node.doc_id)?),
                        int(i64::from(node.vector_ordinal)),
                        int(signed(node.level as u64)?),
                        int(i64::from(node.deleted)),
                        ValueRef::Blob(&bytes),
                    ],
                    control,
                )
            }
            Value::Edge => {
                if address.identity.family() != Family::HNSWEdges {
                    return Err(invalid());
                }
                unsigned(address.numbers[0])?;
                level(address.numbers[1])?;
                unsigned(address.numbers[2])?;
                encode_row(
                    &[
                        row[0],
                        row[1],
                        int(address.numbers[0]),
                        int(address.numbers[1]),
                        int(address.numbers[2]),
                    ],
                    control,
                )
            }
        }
    }
}

pub(super) fn metadata_key(
    owner: NativeRecordOwner,
    field: &str,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    Identity::new(Family::HNSWIndexes, owner)?
        .encode_key(&[ValueRef::Text(field.as_bytes())], control)
}
