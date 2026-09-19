//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key/Value HNSW JSON records with bounded decoded and encoded buffers.

use super::{
    codec::decode_value,
    hnsw_persistence::{
        checked_level, metadata_from_graph, metadata_header, PersistedHNSWMetadata,
        PersistedHNSWNode,
    },
    record_json,
    vector_records::{ordinal, tail, vector_bytes, GUARD},
    TAG_HNSW_METADATA, TAG_HNSW_NODE, TAG_VECTOR,
};
use crate::{
    hnsw_index::HNSWNodeSnapshot,
    mvcc::{
        HNSWRecordHeader, HNSWRecordKey as Key, HNSWRecordLayout, HNSWRecordValue as Value,
        VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};
use serde_json::value::RawValue;
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};

pub struct KeyValueHNSWRecords;
impl HNSWRecordLayout for KeyValueHNSWRecords {
    fn metadata_key(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        control.cancellation().check()?;
        let count = match key.first() {
            Some(&TAG_HNSW_METADATA) => 0,
            Some(&TAG_HNSW_NODE) => 1,
            _ => return Ok(None),
        };
        let (end, _) = tail(key, key[0], count)?;
        let mut output = BudgetedVec::new(control.memory());
        output.extend_from_slice(&key[..end])?;
        output[0] = TAG_HNSW_METADATA;
        Ok(Some(output))
    }
    fn key(
        &self,
        metadata: &[u8],
        address: Key,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        control.cancellation().check()?;
        tail(metadata, TAG_HNSW_METADATA, 0)?;
        let mut key = BudgetedVec::new(control.memory());
        key.extend_from_slice(metadata)?;
        match address {
            Key::Structure => {
                key[0] = GUARD;
                key.push(0)?;
            }
            Key::Document(document) => {
                key[0] = GUARD;
                key.push(1)?;
                key.extend_from_slice(&document.to_be_bytes())?;
            }
            Key::Vectors => key[0] = TAG_VECTOR,
            Key::Nodes => key[0] = TAG_HNSW_NODE,
            Key::Node(node) => {
                key[0] = TAG_HNSW_NODE;
                key.extend_from_slice(&node.to_be_bytes())?;
            }
            Key::Edge { .. } => {
                return Err(VersionError::InvalidEncoding(
                    "KeyValue HNSW adjacency is embedded in nodes",
                ))
            }
        }
        Ok(key)
    }
    fn edges_prefix(
        &self,
        metadata: &[u8],
        _source: Option<u64>,
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        control.cancellation().check()?;
        tail(metadata, TAG_HNSW_METADATA, 0)?;
        Ok(None)
    }
    fn header(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<HNSWRecordHeader> {
        control.cancellation().check()?;
        tail(key, TAG_HNSW_METADATA, 0)?;
        let meta: PersistedHNSWMetadata = decode_value(value)?;
        Ok(metadata_header(&meta)?)
    }

    fn vector_id(&self, key: &[u8], control: &StorageReadControl) -> VersionResult<(DocId, u32)> {
        control.cancellation().check()?;
        let (_, [document, order]) = tail(key, TAG_VECTOR, 2)?;
        Ok((document, ordinal(order)?))
    }
    fn vector(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32, BudgetedVec<f32>)> {
        let (document, order) = self.vector_id(key, control)?;
        Ok((document, order, vector_bytes(value, control)?))
    }
    fn node(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Budgeted<HNSWNodeSnapshot>> {
        control.cancellation().check()?;
        let (_, [id, _]) = tail(key, TAG_HNSW_NODE, 1)?;
        let mut memory = control.memory().reserve(size_of::<HNSWNodeSnapshot>())?;
        let node: PersistedHNSWNode<&RawValue, &RawValue> =
            serde_json::from_slice(value).map_err(crate::StorageBackendError::from)?;
        if id != node.node_id {
            return Err(VersionError::InvalidEncoding(
                "HNSW node identity disagrees with its key",
            ));
        }
        let level = checked_level(node.level, "HNSW node level")?;
        let vector = record_json::array::<f32>(node.raw_vector.get(), control)?;
        let layers = record_json::array::<&RawValue>(node.neighbors.get(), control)?;
        if layers.len() != level + 1 {
            return Err(VersionError::InvalidEncoding(
                "HNSW adjacency does not match its level",
            ));
        }
        let mut neighbors = BudgetedVec::new(control.memory());
        for layer in layers.iter() {
            neighbors.reserve(1)?;
            let (edges, retained) = record_json::array::<u64>(layer.get(), control)?.into_parts();
            memory.absorb(retained);
            neighbors.push(edges)?;
        }
        let (raw_vector, retained) = vector.into_parts();
        memory.absorb(retained);
        let (neighbors, retained) = neighbors.into_parts();
        memory.absorb(retained);
        Ok(Budgeted::new(
            HNSWNodeSnapshot {
                node_id: id,
                doc_id: node.doc_id,
                vector_ordinal: node.vector_ordinal,
                raw_vector,
                level,
                deleted: node.deleted,
                neighbors,
            },
            memory,
        ))
    }
    fn edge(
        &self,
        _key: &[u8],
        _value: &[u8],
        _control: &StorageReadControl,
    ) -> VersionResult<(u64, usize, u64)> {
        Err(VersionError::InvalidEncoding(
            "KeyValue HNSW adjacency is embedded in nodes",
        ))
    }
    fn encode(
        &self,
        key: &[u8],
        template: &[u8],
        value: Value<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        control.cancellation().check()?;
        match value {
            Value::Header { meta, revision } => {
                let header = self.header(key, template, control)?;
                record_json::encode(
                    &metadata_from_graph(
                        header.dimensions,
                        header.params,
                        meta,
                        revision.ok_or(VersionError::InvalidEncoding(
                            "missing HNSW mutation counter",
                        ))?,
                    )?,
                    control,
                )
            }
            Value::Node(node) => {
                let (_, [id, _]) = tail(key, TAG_HNSW_NODE, 1)?;
                if id != node.node_id {
                    return Err(VersionError::InvalidEncoding(
                        "HNSW node identity disagrees with its key",
                    ));
                }
                record_json::encode(&PersistedHNSWNode::try_from(node)?, control)
            }
            Value::Edge => Err(VersionError::InvalidEncoding(
                "KeyValue HNSW adjacency is embedded in nodes",
            )),
        }
    }
}
