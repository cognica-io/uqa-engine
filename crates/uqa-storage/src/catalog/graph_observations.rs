//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical graph payload addresses shared by semantic readers and evaluated provider mutations.

use super::graph_identifiers::GraphIdentifierNamespace;
use crate::mvcc::{SerializableKeySpace, SerializablePredicate};
use crate::{GraphEntityKind, KeyValueBatch, StorageBackendResult};

/// A global entity may belong to several named graphs. Payload reads and writes meet at its original physical scope and clear generation, while vertex and edge identities remain disjoint.
#[derive(Clone, Copy)]
pub struct GraphEntityKey {
    object: [u8; 16],
    key: [u8; 10],
}

impl GraphEntityKey {
    pub fn new(namespace: GraphIdentifierNamespace, kind: GraphEntityKind, id: u64) -> Self {
        let mut key = [0; 10];
        key[0] = b'p';
        key[1] = match kind {
            GraphEntityKind::Vertex => b'v',
            GraphEntityKind::Edge => b'e',
        };
        key[2..].copy_from_slice(&id.to_be_bytes());
        Self {
            object: namespace.serializable_entity_object(),
            key,
        }
    }

    pub fn predicate(&self) -> SerializablePredicate<'_> {
        SerializablePredicate::point(self.object, SerializableKeySpace::Graph, &self.key)
    }

    pub fn observe_write(&self, batch: &mut dyn KeyValueBatch) -> StorageBackendResult<()> {
        batch.observe_serializable_write(self.predicate())
    }
}
