//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical selector intervals are independent of provider lookup layouts and entity properties.

use std::ops::Bound;

use sha2::{Digest, Sha256};

use super::GraphIdentifierNamespace;
use crate::mvcc::{SerializableKeySpace, SerializablePredicate};
use crate::{GraphEntityFilter, GraphEntityKind, KeyValueBatch, StorageBackendResult};

/// The source fields that affect graph identity selection or adjacency. Properties belong to separate entity payload observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphEntityTopology<'a> {
    Vertex {
        label: &'a str,
    },
    Edge {
        label: &'a str,
        source: u64,
        target: u64,
    },
}

impl GraphEntityTopology<'_> {
    pub fn kind(self) -> GraphEntityKind {
        match self {
            Self::Vertex { .. } => GraphEntityKind::Vertex,
            Self::Edge { .. } => GraphEntityKind::Edge,
        }
    }

    /// Supply each supported conjunction using the already evaluated source fields. Every key is bounded, and the batch retains the original allowance and participant.
    pub fn observe_write(
        self,
        namespace: GraphIdentifierNamespace,
        id: u64,
        graph: Option<&str>,
        batch: &mut dyn KeyValueBatch,
    ) -> StorageBackendResult<()> {
        let (label, endpoints) = match self {
            Self::Vertex { label } => (label, None),
            Self::Edge {
                label,
                source,
                target,
            } => (label, Some((source, target))),
        };
        for mask in 0..if endpoints.is_some() { 8 } else { 2 } {
            let mut filter = GraphEntityFilter::new(self.kind(), graph);
            filter.label = (mask & 1 != 0).then_some(label);
            if let Some((source, target)) = endpoints {
                filter.source = (mask & 2 != 0).then_some(source);
                filter.target = (mask & 4 != 0).then_some(target);
            }
            let key = GraphSelectionKey::new(namespace, filter, None)?;
            let mut point = key.lower;
            point[33..].copy_from_slice(&id.to_be_bytes());
            batch.observe_serializable_write(SerializablePredicate::point(
                key.object,
                SerializableKeySpace::Graph,
                &point,
            ))?;
        }
        Ok(())
    }
}

/// A conjunction over graph identities, including empty results and the requested page suffix. Hashing the selector bounds retained key storage independently of label/name length; ordered entity IDs retain their range semantics.
pub struct GraphSelectionKey {
    object: [u8; 16],
    lower: [u8; 41],
    upper: [u8; 41],
    after: bool,
}

impl GraphSelectionKey {
    pub fn new(
        namespace: GraphIdentifierNamespace,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
    ) -> StorageBackendResult<Self> {
        filter.validate()?;
        let mut digest = Sha256::new();
        digest.update([kind_tag(filter.kind)]);
        text(&mut digest, filter.graph);
        text(&mut digest, filter.label);
        for id in [filter.source, filter.target] {
            digest.update([u8::from(id.is_some())]);
            digest.update(id.unwrap_or_default().to_be_bytes());
        }
        let mut lower = [0; 41];
        lower[0] = b's';
        lower[1..33].copy_from_slice(&digest.finalize());
        let mut upper = lower;
        lower[33..].copy_from_slice(&after.unwrap_or_default().to_be_bytes());
        upper[33..].copy_from_slice(&u64::MAX.to_be_bytes());
        Ok(Self {
            object: namespace.serializable_entity_object(),
            lower,
            upper,
            after: after.is_some(),
        })
    }

    pub fn predicate(&self) -> SerializablePredicate<'_> {
        SerializablePredicate::range(
            self.object,
            SerializableKeySpace::Graph,
            if self.after {
                Bound::Excluded(&self.lower)
            } else {
                Bound::Included(&self.lower)
            },
            Bound::Included(&self.upper),
        )
    }
}

/// One entity's graph memberships, separated from its payload and from other entities of either kind.
pub struct GraphMembershipKey {
    object: [u8; 16],
    lower: [u8; 42],
    upper: [u8; 42],
    point: bool,
}

impl GraphMembershipKey {
    pub fn new(
        namespace: GraphIdentifierNamespace,
        kind: GraphEntityKind,
        id: u64,
        graph: Option<&str>,
    ) -> Self {
        let mut lower = [0; 42];
        lower[0] = b'm';
        lower[1] = kind_tag(kind);
        lower[2..10].copy_from_slice(&id.to_be_bytes());
        let mut upper = lower;
        if let Some(graph) = graph {
            lower[10..].copy_from_slice(&Sha256::digest(graph.as_bytes()));
        } else {
            upper[10..].fill(u8::MAX);
        }
        Self {
            object: namespace.serializable_entity_object(),
            lower,
            upper,
            point: graph.is_some(),
        }
    }

    pub fn predicate(&self) -> SerializablePredicate<'_> {
        if self.point {
            SerializablePredicate::point(self.object, SerializableKeySpace::Graph, &self.lower)
        } else {
            SerializablePredicate::range(
                self.object,
                SerializableKeySpace::Graph,
                Bound::Included(&self.lower),
                Bound::Included(&self.upper),
            )
        }
    }
}

fn kind_tag(kind: GraphEntityKind) -> u8 {
    match kind {
        GraphEntityKind::Vertex => b'v',
        GraphEntityKind::Edge => b'e',
    }
}

fn text(digest: &mut Sha256, value: Option<&str>) {
    digest.update([u8::from(value.is_some())]);
    let value = value.unwrap_or_default().as_bytes();
    digest.update(
        u64::try_from(value.len())
            .expect("graph selector length")
            .to_be_bytes(),
    );
    digest.update(value);
}
