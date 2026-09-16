//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Indexed graph reads. Entity payloads are fetched separately from bounded
//! identity scans so traversal never requires a resident graph replica.

use crate::{StorageBackendError, StorageBackendResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEntityKind {
    Vertex,
    Edge,
}

impl GraphEntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vertex => "vertex",
            Self::Edge => "edge",
        }
    }
}

/// Conjunctive, storage-side filters over graph entity identities. `None`
/// means no restriction; an empty label is an ordinary exact label value.
#[derive(Debug, Clone, Copy)]
pub struct GraphEntityFilter<'a> {
    pub kind: GraphEntityKind,
    pub graph: Option<&'a str>,
    pub label: Option<&'a str>,
    pub source: Option<u64>,
    pub target: Option<u64>,
}

/// Primary identity index before applying the remaining conjunction. Adjacency and label selections take precedence over graph-wide membership scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEntitySelector<'a> {
    All,
    Graph(&'a str),
    Label(&'a str),
    Source(u64),
    Target(u64),
}

impl<'a> GraphEntityFilter<'a> {
    pub fn new(kind: GraphEntityKind, graph: Option<&'a str>) -> Self {
        Self {
            kind,
            graph,
            label: None,
            source: None,
            target: None,
        }
    }

    pub fn validate(self) -> StorageBackendResult<()> {
        if self.kind == GraphEntityKind::Vertex && (self.source.is_some() || self.target.is_some())
        {
            return Err(StorageBackendError::Other(
                "vertex scans cannot have edge endpoint filters".into(),
            ));
        }
        Ok(())
    }

    pub fn selector(self) -> StorageBackendResult<GraphEntitySelector<'a>> {
        self.validate()?;
        Ok(if let Some(source) = self.source {
            GraphEntitySelector::Source(source)
        } else if let Some(target) = self.target {
            GraphEntitySelector::Target(target)
        } else if let Some(label) = self.label {
            GraphEntitySelector::Label(label)
        } else if let Some(graph) = self.graph {
            GraphEntitySelector::Graph(graph)
        } else {
            GraphEntitySelector::All
        })
    }
}

/// Upper bound on one graph identity page; callers advance with the last id.
pub const MAX_GRAPH_ID_PAGE: usize = 4096;

pub fn validate_graph_page(limit: usize) -> StorageBackendResult<()> {
    if !(1..=MAX_GRAPH_ID_PAGE).contains(&limit) {
        return Err(StorageBackendError::Other(format!(
            "graph identity page size must be in 1..={MAX_GRAPH_ID_PAGE}"
        )));
    }
    Ok(())
}
