//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog presence and definition addresses are independent of graph entity generations.

use std::ops::Bound;

use sha2::{Digest, Sha256};

use super::GraphIdentifierNamespace;
use crate::mvcc::{SerializableKeySpace, SerializablePredicate};

#[derive(Clone, Copy)]
pub enum GraphDefinitionKind {
    NamedGraph,
    PathIndex,
    LabelRegistry,
}

/// A point for one canonical catalog name, or a range for the complete name listing. Fixed size hashes bound retained observations without retaining arbitrary catalog strings.
pub struct GraphDefinitionKey {
    object: [u8; 16],
    lower: [u8; 33],
    upper: [u8; 33],
}

impl GraphDefinitionKey {
    pub fn new(
        namespace: GraphIdentifierNamespace,
        kind: GraphDefinitionKind,
        name: Option<&str>,
    ) -> Self {
        let tag = match kind {
            GraphDefinitionKind::NamedGraph => b'n',
            GraphDefinitionKind::PathIndex => b'i',
            GraphDefinitionKind::LabelRegistry => b'r',
        };
        let mut lower = [0; 33];
        let mut upper = [u8::MAX; 33];
        lower[0] = tag;
        upper[0] = tag;
        if let Some(name) = name {
            lower[1..].copy_from_slice(&Sha256::digest(name.as_bytes()));
            upper = lower;
        }
        Self {
            object: namespace.serializable_scope_object(),
            lower,
            upper,
        }
    }

    pub fn predicate(&self) -> SerializablePredicate<'_> {
        if self.lower == self.upper {
            return SerializablePredicate::point(
                self.object,
                SerializableKeySpace::Graph,
                &self.lower,
            );
        }
        SerializablePredicate::range(
            self.object,
            SerializableKeySpace::Graph,
            Bound::Included(&self.lower),
            Bound::Included(&self.upper),
        )
    }
}
