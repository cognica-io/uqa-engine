//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing Key/Value graph addresses and path validity payloads for common commit resolution.

use uqa_core::memory::BudgetedVec;

use crate::mvcc::{
    DatabaseId, GraphRecordKey, GraphRecordLayout, MergedRecordSnapshot, VersionError,
    VersionResult,
};
use crate::{read_control::StorageReadControl, GraphEntityKind};

use super::codec::{key_segment_length, read_segment, read_u64};
use super::{
    TAG_EDGE, TAG_GRAPH_LOOKUP, TAG_GRAPH_MEMBERSHIP, TAG_METADATA, TAG_NAMED_GRAPH,
    TAG_PATH_INDEX, TAG_PATH_INDEX_DATA, TAG_VERTEX,
};

pub struct KeyValueGraphRecords;

fn owns_path(
    view: &MergedRecordSnapshot,
    validity_key: &[u8],
    graph: &str,
    control: &StorageReadControl,
) -> VersionResult<bool> {
    if !KeyValueGraphRecords.is_validity_key(validity_key)? {
        return Err(VersionError::InvalidEncoding("invalid path validity key"));
    }
    let mut key = BudgetedVec::new(control.memory());
    key.extend_from_slice(validity_key)?;
    key[1] = b's';
    Ok(view
        .get(&key, control)?
        .as_ref()
        .and_then(|record| record.value())
        == Some(graph.as_bytes()))
}

fn segment(key: &mut BudgetedVec<u8>, parts: &[&[u8]]) -> VersionResult<()> {
    let size = parts.iter().try_fold(0_usize, |length, part| {
        length
            .checked_add(part.len())
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)
    })?;
    key.extend_from_slice(&key_segment_length(size)?.to_be_bytes())?;
    for part in parts {
        key.extend_from_slice(part)?;
    }
    Ok(())
}

fn text<'a>(key: &'a [u8], offset: &mut usize) -> VersionResult<&'a str> {
    std::str::from_utf8(read_segment(key, offset)?)
        .map_err(|_| VersionError::InvalidEncoding("graph key text is not UTF-8"))
}

fn complete(key: &[u8], offset: usize) -> VersionResult<()> {
    if key.len() != offset {
        return Err(VersionError::InvalidEncoding(
            "graph key has trailing bytes",
        ));
    }
    Ok(())
}

fn kind(name: &str) -> VersionResult<GraphEntityKind> {
    match name {
        "vertex" => Ok(GraphEntityKind::Vertex),
        "edge" => Ok(GraphEntityKind::Edge),
        _ => Err(VersionError::InvalidEncoding(
            "unsupported graph entity kind",
        )),
    }
}

impl GraphRecordLayout for KeyValueGraphRecords {
    fn key(
        &self,
        _: DatabaseId,
        address: GraphRecordKey<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        control.check()?;
        let mut key = BudgetedVec::new(control.memory());
        match address {
            GraphRecordKey::Entity(kind, id) => {
                key.push(if kind == GraphEntityKind::Vertex {
                    TAG_VERTEX
                } else {
                    TAG_EDGE
                })?;
                key.extend_from_slice(&id.to_be_bytes())?;
            }
            GraphRecordKey::EntityMemberships(kind, id) => {
                key.extend_from_slice(&[TAG_GRAPH_LOOKUP, b'm'])?;
                segment(&mut key, &[kind.as_str().as_bytes()])?;
                key.extend_from_slice(&id.to_be_bytes())?;
            }
            GraphRecordKey::GraphMemberships(graph) => {
                key.push(TAG_GRAPH_MEMBERSHIP)?;
                segment(&mut key, &[graph.as_bytes()])?;
            }
            GraphRecordKey::GraphPaths(graph) => {
                key.extend_from_slice(&[TAG_PATH_INDEX_DATA, b'g'])?;
                segment(&mut key, &[graph.as_bytes()])?;
            }
            GraphRecordKey::GraphName(graph) => {
                key.push(TAG_NAMED_GRAPH)?;
                segment(&mut key, &[graph.as_bytes()])?;
            }
            GraphRecordKey::LabelRegistry(graph) => {
                key.push(TAG_METADATA)?;
                segment(&mut key, &[b"graph_label_registry::", graph.as_bytes()])?;
            }
            GraphRecordKey::PathDefinition(index) => {
                key.push(TAG_PATH_INDEX)?;
                segment(&mut key, &[index.as_bytes()])?;
            }
            GraphRecordKey::PathValidity(index) => {
                key.extend_from_slice(&[TAG_PATH_INDEX_DATA, b'v'])?;
                segment(&mut key, &[index.as_bytes()])?;
            }
        }
        Ok(key)
    }

    fn is_validity_key(&self, key: &[u8]) -> VersionResult<bool> {
        if !key.starts_with(&[TAG_PATH_INDEX_DATA, b'v']) {
            return Ok(false);
        }
        let mut offset = 2;
        text(key, &mut offset)?;
        complete(key, offset)?;
        Ok(true)
    }

    fn membership_graph(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        if !key.starts_with(&[TAG_GRAPH_LOOKUP, b'm']) {
            return Err(VersionError::InvalidEncoding(
                "invalid reverse graph membership key",
            ));
        }
        let mut offset = 2;
        kind(text(key, &mut offset)?)?;
        read_u64(key, &mut offset)?;
        let graph = text(key, &mut offset)?;
        complete(key, offset)?;
        let mut result = BudgetedVec::new(control.memory());
        result.extend_from_slice(graph.as_bytes())?;
        Ok(result)
    }

    fn membership_entity(&self, key: &[u8]) -> VersionResult<(GraphEntityKind, u64)> {
        if key.first() != Some(&TAG_GRAPH_MEMBERSHIP) {
            return Err(VersionError::InvalidEncoding(
                "invalid graph membership key",
            ));
        }
        let mut offset = 1;
        text(key, &mut offset)?;
        let kind = kind(text(key, &mut offset)?)?;
        let id = read_u64(key, &mut offset)?;
        complete(key, offset)?;
        Ok((kind, id))
    }

    fn path_validity_key(
        &self,
        view: &MergedRecordSnapshot,
        database: DatabaseId,
        graph: &str,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        if !key.starts_with(&[TAG_PATH_INDEX_DATA, b'g']) || !value.is_empty() {
            return Err(VersionError::InvalidEncoding(
                "invalid graph path directory entry",
            ));
        }
        let mut offset = 2;
        let owner = text(key, &mut offset)?;
        let index = text(key, &mut offset)?;
        complete(key, offset)?;
        if owner != graph {
            return Ok(None);
        }
        let key = self.key(database, GraphRecordKey::PathValidity(index), control)?;
        Ok(owns_path(view, &key, graph, control)?.then_some(key))
    }

    fn invalidate(
        &self,
        _: &[u8],
        _: &[u8],
        _: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        Ok(None)
    }

    fn is_published(
        &self,
        view: &MergedRecordSnapshot,
        key: &[u8],
        value: &[u8],
        graph: &str,
        definition: &str,
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        Ok(value == definition.as_bytes() && owns_path(view, key, graph, control)?)
    }

    fn definition_matches(
        &self,
        _: &[u8],
        value: &[u8],
        definition: &str,
        _: &StorageReadControl,
    ) -> VersionResult<bool> {
        Ok(value == definition.as_bytes())
    }
}
