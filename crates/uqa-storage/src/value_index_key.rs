//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical B-tree namespaces distinguish column accelerators from named SQL indexes.

/// A stored evaluated key distinguishes an unbuilt index, an absent row and a present SQL NULL. Consumers must not recompute an old expression against a newer routine definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueIndexEntry {
    Unbuilt,
    Absent,
    Present(uqa_core::Value),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ValueIndexKey {
    Column(String),
    Index(String),
}

impl ValueIndexKey {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Column(name) | Self::Index(name) => name,
        }
    }
}

impl From<&str> for ValueIndexKey {
    fn from(name: &str) -> Self {
        Self::Column(name.into())
    }
}

impl From<String> for ValueIndexKey {
    fn from(name: String) -> Self {
        Self::Column(name)
    }
}

impl std::fmt::Display for ValueIndexKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Column(name) => write!(formatter, "column {name}"),
            Self::Index(name) => write!(formatter, "index {name}"),
        }
    }
}
