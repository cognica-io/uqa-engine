//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider-neutral metadata identities for graph entity and membership lifetimes.

use crate::GraphEntityKind;

/// Internal lifetime state does not change user catalog definitions or their cache revisions.
pub const METADATA_PREFIX: &str = "uqa.graph.guard.v1:";

/// A global entity's existence and, for edges, its endpoint pair. Properties do not alter this identity's lifetime.
pub struct GraphRecordGuard {
    kind: GraphEntityKind,
    id: u64,
}

impl GraphRecordGuard {
    pub fn new(kind: GraphEntityKind, id: u64) -> Self {
        Self { kind, id }
    }

    pub fn from_kind_name(kind: &str, id: u64) -> Option<Self> {
        let kind = match kind {
            "vertex" => GraphEntityKind::Vertex,
            "edge" => GraphEntityKind::Edge,
            _ => return None,
        };
        Some(Self::new(kind, id))
    }

    pub fn lifetime(&self) -> String {
        format!("{METADATA_PREFIX}entity:{}:{}", self.kind.as_str(), self.id)
    }

    pub fn references(&self) -> String {
        format!(
            "{METADATA_PREFIX}references:{}:{}",
            self.kind.as_str(),
            self.id
        )
    }

    pub fn membership_references(&self, graph: &str) -> String {
        format!(
            "{METADATA_PREFIX}membership:{}:{}:{graph}",
            self.kind.as_str(),
            self.id
        )
    }
}
