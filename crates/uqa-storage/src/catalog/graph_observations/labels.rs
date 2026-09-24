//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical label registry projection excludes autonomous counters from semantic label definitions.

mod projection;

use sha2::{Digest, Sha256};

use super::{GraphDefinitionKey, GraphDefinitionKind, GraphIdentifierNamespace};
use crate::mvcc::{SerializableKeySpace, SerializablePredicate};
use crate::{read_control::StorageReadControl, KeyValueBatch, StorageBackendResult};

#[derive(Clone, Copy)]
pub enum GraphLabelName<'a> {
    Named(&'a str),
    Reserved(u32),
}

/// Labels have a graph-specific logical object. An opaque legacy registry change can invalidate that object without interfering with another graph's label readers.
pub struct GraphLabelDefinitionKey {
    object: [u8; 16],
    key: Option<[u8; 33]>,
}

impl GraphLabelDefinitionKey {
    pub fn new(
        namespace: GraphIdentifierNamespace,
        graph: &str,
        label: Option<GraphLabelName<'_>>,
    ) -> Self {
        let mut digest = Sha256::new();
        digest.update(namespace.serializable_scope_object());
        digest.update(b"graph-labels");
        digest.update(graph.as_bytes());
        let mut object = [0; 16];
        object.copy_from_slice(&digest.finalize()[..16]);
        if object == [0; 16] {
            object[15] = 1;
        }
        let key = label.map(|label| {
            let mut key = [0; 33];
            match label {
                GraphLabelName::Named(name) => {
                    key[0] = b'n';
                    key[1..].copy_from_slice(&Sha256::digest(name.as_bytes()));
                }
                GraphLabelName::Reserved(id) => {
                    key[0] = b'd';
                    key[1..5].copy_from_slice(&id.to_be_bytes());
                }
            }
            key
        });
        Self { object, key }
    }

    pub fn predicate(&self) -> SerializablePredicate<'_> {
        self.key.as_ref().map_or_else(
            || SerializablePredicate::object(self.object),
            |key| SerializablePredicate::point(self.object, SerializableKeySpace::Graph, key),
        )
    }
}

/// Compare already evaluated registry records under the original allowance. Catalog APIs continue to accept opaque legacy records; their replacement conservatively changes the selected graph's whole label object.
pub fn observe_label_registry_change(
    namespace: GraphIdentifierNamespace,
    graph: &str,
    old: Option<&str>,
    new: Option<&str>,
    batch: &mut dyn KeyValueBatch,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    if batch.serializable_participant().is_none() || old == new {
        return Ok(());
    }
    control.check()?;
    batch.observe_serializable_write(
        GraphDefinitionKey::new(namespace, GraphDefinitionKind::LabelRegistry, Some(graph))
            .predicate(),
    )?;
    let key = GraphLabelDefinitionKey::new(namespace, graph, None);
    let (Some(old), Some(new)) = (
        projection::Registry::decode(old, control)?,
        projection::Registry::decode(new, control)?,
    ) else {
        return batch.observe_serializable_write(key.predicate());
    };
    for id in [1_u32, 2] {
        if old.dropped(id) != new.dropped(id) || old.reserved_alias(id) != new.reserved_alias(id) {
            batch.observe_serializable_write(
                GraphLabelDefinitionKey::new(namespace, graph, Some(GraphLabelName::Reserved(id)))
                    .predicate(),
            )?;
        }
    }
    old.visit_changes(&new, control, |name| {
        let mut bytes = [b'n'; 33];
        bytes[1..].copy_from_slice(name);
        batch.observe_serializable_write(SerializablePredicate::point(
            key.object,
            SerializableKeySpace::Graph,
            &bytes,
        ))
    })
}
