//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native addressing and row encoding for the common graph commit resolver.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    mvcc::{DatabaseId, GraphRecordKey, GraphRecordLayout, MergedRecordSnapshot, VersionResult},
    read_control::StorageReadControl,
    GraphEntityKind,
};

use super::{
    decode_record, encode_row, invalid, NativeRecordFamily as Family, NativeRecordIdentity,
    NativeRecordNamespace, NativeRecordOwner,
};

fn text(value: &str) -> ValueRef<'_> {
    ValueRef::Text(value.as_bytes())
}

fn number(id: u64) -> VersionResult<ValueRef<'static>> {
    Ok(ValueRef::Integer(i64::try_from(id).map_err(|_| {
        invalid("graph identity exceeds SQLite integer range")
    })?))
}

fn components(
    key: &[u8],
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<ValueRef<'static>>> {
    let mut values = BudgetedVec::new(control.memory());
    NativeRecordIdentity::visit_key_components(key, control, |_, value| {
        // Only integer components are returned; text is decoded separately while its borrowed key is available.
        if let ValueRef::Integer(value) = value {
            values.push(ValueRef::Integer(value))?;
        }
        Ok(())
    })?;
    Ok(values)
}

fn key_text(
    key: &[u8],
    column: usize,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    let mut bytes = BudgetedVec::new(control.memory());
    let mut found = false;
    NativeRecordIdentity::visit_key_components(key, control, |slot, value| {
        if slot == column {
            bytes.extend_from_slice(
                value
                    .as_str()
                    .map_err(|_| invalid("native graph key is not text"))?
                    .as_bytes(),
            )?;
            found = true;
        }
        Ok(())
    })?;
    if !found {
        return Err(invalid("native graph key component is missing"));
    }
    Ok(bytes)
}

impl GraphRecordLayout for NativeRecordNamespace {
    fn key(
        &self,
        _database: DatabaseId,
        address: GraphRecordKey<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let encode = |family, parts: &[ValueRef<'_>], full| {
            let identity = NativeRecordIdentity::new(family, NativeRecordOwner::Database(self.0))?;
            if full {
                identity.encode_key(parts, control)
            } else {
                identity.encode_prefix(parts, control)
            }
        };
        match address {
            GraphRecordKey::Entity(kind, id) => encode(
                if kind == GraphEntityKind::Vertex {
                    Family::GraphVertices
                } else {
                    Family::GraphEdges
                },
                &[number(id)?],
                true,
            ),
            GraphRecordKey::EntityMemberships(kind, id) => encode(
                Family::GraphMembership,
                &[text(kind.as_str()), number(id)?],
                false,
            ),
            GraphRecordKey::GraphMemberships(graph) => encode(
                Family::GraphLookups,
                &[text("member"), text(graph), ValueRef::Integer(0)],
                false,
            ),
            GraphRecordKey::GraphPaths(graph) => encode(
                Family::GraphLookups,
                &[text("path"), text(graph), ValueRef::Integer(0)],
                false,
            ),
            GraphRecordKey::GraphName(graph) => encode(Family::NamedGraphs, &[text(graph)], true),
            GraphRecordKey::LabelRegistry(graph) => {
                let mut name = BudgetedVec::new(control.memory());
                name.extend_from_slice(b"graph_label_registry::")?;
                name.extend_from_slice(graph.as_bytes())?;
                encode(Family::Metadata, &[ValueRef::Text(&name)], true)
            }
            GraphRecordKey::PathDefinition(index) => {
                encode(Family::PathIndexes, &[text(index)], true)
            }
            GraphRecordKey::PathValidity(index) => {
                encode(Family::GraphPathIndexState, &[text(index)], true)
            }
        }
    }

    fn is_validity_key(&self, key: &[u8]) -> VersionResult<bool> {
        Ok(NativeRecordIdentity::decode(key)?.family() == Family::GraphPathIndexState)
    }

    fn membership_graph(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        if NativeRecordIdentity::decode_full(key, control)?.family() != Family::GraphMembership {
            return Err(invalid("native graph membership has another record family"));
        }
        key_text(key, 2, control)
    }

    fn membership_entity(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(GraphEntityKind, u64)> {
        if NativeRecordIdentity::decode_full(key, control)?.family() != Family::GraphLookups
            || &*key_text(key, 0, control)? != b"member"
        {
            return Err(invalid("native graph membership selector has another kind"));
        }
        let kind = match &*key_text(key, 3, control)? {
            b"vertex" => GraphEntityKind::Vertex,
            b"edge" => GraphEntityKind::Edge,
            _ => return Err(invalid("unsupported native graph membership kind")),
        };
        let integers = components(key, control)?;
        let id = integers
            .last()
            .ok_or_else(|| invalid("native graph membership identity is missing"))?
            .as_i64()
            .map_err(|_| invalid("native graph membership identity is not an integer"))?;
        Ok((
            kind,
            u64::try_from(id)
                .map_err(|_| invalid("native graph membership identity is negative"))?,
        ))
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
        let (identity, row) = decode_record(key, value, control)?;
        if identity.family() != Family::GraphLookups
            || row[0] != text("path")
            || row[1] != text(graph)
        {
            return Err(invalid("invalid native graph path directory entry"));
        }
        let index = row[3]
            .as_str()
            .map_err(|_| invalid("native path identity is not text"))?;
        let key = self.key(database, GraphRecordKey::PathValidity(index), control)?;
        let record = view.get(&key, control)?;
        if let Some(value) = record.as_ref().and_then(|row| row.value()) {
            let (_, row) = decode_record(&key, value, control)?;
            if row[1] == text(graph) {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    fn invalidate(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        let (_, mut row) = decode_record(key, value, control)?;
        row[3] = ValueRef::Integer(0);
        Ok(Some(encode_row(&row, control)?))
    }

    fn is_published(
        &self,
        _: &MergedRecordSnapshot,
        key: &[u8],
        value: &[u8],
        graph: &str,
        definition: &str,
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        let (_, row) = decode_record(key, value, control)?;
        Ok(row[1] == text(graph) && row[2] == text(definition) && row[3] == ValueRef::Integer(1))
    }

    fn definition_matches(
        &self,
        key: &[u8],
        value: &[u8],
        definition: &str,
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        let (_, row) = decode_record(key, value, control)?;
        Ok(row[1] == text(definition))
    }
}
