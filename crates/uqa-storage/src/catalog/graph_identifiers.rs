//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical allocation addresses and export projection for graph registry records. Graph owns label validation and graphid allocation rules.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::{GraphEntityKind, KeyValueBatch, StorageBackendResult};

#[derive(Clone, Copy)]
pub struct GraphIdentifierNamespace {
    scope: [u8; 32],
    generation: [u8; 16],
}

impl GraphIdentifierNamespace {
    pub fn new(scope: Option<&str>, generation: [u8; 16]) -> Self {
        let mut digest = Sha256::new();
        digest.update([u8::from(scope.is_some())]);
        digest.update(scope.unwrap_or_default().as_bytes());
        Self {
            scope: digest.finalize().into(),
            generation,
        }
    }

    pub fn key(&self, tag: u8, kind: GraphEntityKind, prefix: u32, graph: [u8; 16]) -> [u8; 78] {
        let mut key = [0; 78];
        key[..8].copy_from_slice(b"UQAGRPH1");
        key[8..40].copy_from_slice(&self.scope);
        key[40..56].copy_from_slice(&self.generation);
        key[56] = tag;
        key[57] = match kind {
            GraphEntityKind::Vertex => b'v',
            GraphEntityKind::Edge => b'e',
        };
        key[58..62].copy_from_slice(&prefix.to_be_bytes());
        key[62..].copy_from_slice(&graph);
        key
    }

    /// Legacy names identify only the original incarnation. New graph creation supplies a random identity; rename retains the original identity in its registry.
    pub fn legacy_graph_identity(&self, graph: &str) -> [u8; 16] {
        let mut digest = Sha256::new();
        digest.update(self.key(b'g', GraphEntityKind::Vertex, 0, [0; 16]));
        digest.update(graph.as_bytes());
        let mut identity = [0; 16];
        identity.copy_from_slice(&digest.finalize()[..16]);
        if identity == [0; 16] {
            identity[15] = 1;
        }
        identity
    }

    pub fn label_key(&self, graph: [u8; 16]) -> [u8; 78] {
        self.key(b'l', GraphEntityKind::Vertex, 0, graph)
    }

    /// Logical graph entity observations share the physical entity scope and clear generation, independently of named graph membership or table row identities.
    pub fn serializable_entity_object(&self) -> [u8; 16] {
        let digest = Sha256::digest(self.key(b'r', GraphEntityKind::Vertex, 0, [0; 16]));
        let mut identity = [0; 16];
        identity.copy_from_slice(&digest[..16]);
        if identity == [0; 16] {
            identity[15] = 1;
        }
        identity
    }

    pub fn sequence_key(&self, graph: [u8; 16], label: u32) -> [u8; 78] {
        self.key(b's', GraphEntityKind::Vertex, label, graph)
    }

    pub fn entity_key(&self, kind: GraphEntityKind, prefix: u32) -> [u8; 78] {
        self.key(b'e', kind, prefix, [0; 16])
    }

    pub fn entity_seed_key(&self, kind: GraphEntityKind, prefix: u32) -> [u8; 78] {
        self.key(b'i', kind, prefix, [0; 16])
    }

    pub fn hint_key(&self, kind: GraphEntityKind) -> [u8; 78] {
        self.key(b'h', kind, 0, [0; 16])
    }

    /// Preserve a supplied physical identity before its evaluated record is published. The wire identity has a two-byte prefix and six-byte ordinal; observing one row does not certify that legacy rows were seeded.
    pub fn observe_entity(
        &self,
        batch: &mut dyn KeyValueBatch,
        kind: GraphEntityKind,
        id: u64,
    ) -> StorageBackendResult<()> {
        let bytes = id.to_be_bytes();
        let prefix = u32::from(u16::from_be_bytes([bytes[0], bytes[1]]));
        let ordinal = id & (u64::MAX >> 16);
        batch.observe_identifier(&self.entity_key(kind, prefix), ordinal)?;
        batch.observe_identifier(&self.hint_key(kind), id)
    }

    /// Import durable counters encoded in a graph registry without assigning labels or evaluating graph behavior. Opaque legacy payloads retain their existing catalog contract.
    pub fn observe_registry(
        &self,
        batch: &mut dyn KeyValueBatch,
        graph: &str,
        source: &str,
    ) -> StorageBackendResult<()> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(source) else {
            return Ok(());
        };
        let Some(object) = value.as_object() else {
            return Ok(());
        };
        let identity = object
            .get("allocation_id")
            .map(|value| serde_json::from_value::<[u8; 16]>(value.clone()))
            .transpose()?
            .filter(|id| *id != [0; 16])
            .unwrap_or_else(|| self.legacy_graph_identity(graph));
        if let Some(next) = object
            .get("next_label_id")
            .and_then(serde_json::Value::as_u64)
        {
            batch.observe_identifier(&self.label_key(identity), next.saturating_sub(1))?;
        }
        if let Some(sequences) = object
            .get("sequences")
            .and_then(serde_json::Value::as_object)
        {
            for (label, counter) in sequences {
                let (Ok(label), Some(last)) = (label.parse::<u32>(), counter.as_u64()) else {
                    continue;
                };
                batch.observe_identifier(&self.sequence_key(identity, label), last)?;
                if let Ok(prefix) = u16::try_from(label) {
                    if last != 0 && last <= (u64::MAX >> 16) {
                        let mut bytes = last.to_be_bytes();
                        bytes[..2].copy_from_slice(&prefix.to_be_bytes());
                        let id = u64::from_be_bytes(bytes);
                        let kind = registry_kind(&value, label);
                        for candidate in [GraphEntityKind::Vertex, GraphEntityKind::Edge] {
                            if kind.is_none_or(|kind| kind == candidate) {
                                self.observe_entity(batch, candidate, id)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn registry_kind(value: &serde_json::Value, label: u32) -> Option<GraphEntityKind> {
    match label {
        1 => Some(GraphEntityKind::Vertex),
        2 => Some(GraphEntityKind::Edge),
        _ => value
            .get("labels")?
            .as_object()?
            .iter()
            .find_map(|(name, id)| {
                if id.as_u64() != Some(u64::from(label)) {
                    return None;
                }
                match value
                    .get("kinds")
                    .and_then(|kinds| kinds.get(name))
                    .and_then(serde_json::Value::as_str)
                {
                    Some("v") => Some(GraphEntityKind::Vertex),
                    Some("e") => Some(GraphEntityKind::Edge),
                    _ => None,
                }
            }),
    }
}

/// Copy current autonomous floors into an explicit graph export. Unknown fields and unchanged legacy payloads retain their original representation; this does not publish a catalog write or alter its pinned definition.
pub fn export_registry(
    source: &str,
    graph: &str,
    namespace: GraphIdentifierNamespace,
    mut watermark: impl FnMut(&[u8]) -> StorageBackendResult<Option<u64>>,
) -> StorageBackendResult<String> {
    let mut value: serde_json::Value = if source.is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(source) {
            Ok(value) => value,
            Err(_) => return Ok(source.to_owned()),
        }
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(source.to_owned());
    };
    let identity = object
        .get("allocation_id")
        .map(|value| serde_json::from_value::<[u8; 16]>(value.clone()))
        .transpose()?
        .filter(|id| *id != [0; 16])
        .unwrap_or_else(|| namespace.legacy_graph_identity(graph));
    let mut changed = false;
    if let Some(last) = watermark(&namespace.label_key(identity))? {
        let next = last.checked_add(1).ok_or_else(|| {
            crate::StorageBackendError::Other("graph label watermark overflow".into())
        })?;
        if object
            .get("next_label_id")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|old| old < next)
        {
            object.insert("next_label_id".into(), next.into());
            changed = true;
        }
    }
    let mut labels = BTreeSet::from([1_u32, 2]);
    if let Some(registered) = object.get("labels").and_then(serde_json::Value::as_object) {
        for value in registered.values() {
            if let Some(label) = value.as_u64().and_then(|id| u32::try_from(id).ok()) {
                labels.insert(label);
            }
        }
    }
    if let Some(sequences) = object
        .get("sequences")
        .and_then(serde_json::Value::as_object)
    {
        for label in sequences.keys() {
            if let Ok(label) = label.parse() {
                labels.insert(label);
            }
        }
    }
    for label in labels {
        let Some(last) = watermark(&namespace.sequence_key(identity, label))? else {
            continue;
        };
        let sequences = object
            .entry("sequences")
            .or_insert_with(|| serde_json::json!({}));
        let sequences = sequences.as_object_mut().ok_or_else(|| {
            crate::StorageBackendError::Other("invalid graph registry sequences".into())
        })?;
        let key = label.to_string();
        if sequences
            .get(&key)
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|old| old < last)
        {
            sequences.insert(key, last.into());
            changed = true;
        }
    }
    if changed {
        object.insert("allocation_id".into(), serde_json::to_value(identity)?);
        Ok(serde_json::to_string(&value)?)
    } else {
        Ok(source.to_owned())
    }
}
