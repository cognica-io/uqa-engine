//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index type definitions for the storage layer.
//!
//! Mirrors UQA `storage/index_types`. Values map onto the wire-
//! visible names that flow through `CREATE INDEX ... USING <name>`
//! at SQL parse time.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexType {
    BTree,
    Gin,
    Inverted,
    /// Reserved for catalog compatibility with older metadata. SQL
    /// normalizes `USING hnsw` onto the IVF backend.
    HNSW,
    IVF,
    Graph,
    RTree,
}

impl IndexType {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "btree" => Some(IndexType::BTree),
            "gin" => Some(IndexType::Gin),
            "inverted" => Some(IndexType::Inverted),
            "ivf" | "hnsw" => Some(IndexType::IVF),
            "graph" => Some(IndexType::Graph),
            "rtree" | "r*tree" => Some(IndexType::RTree),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            IndexType::BTree => "btree",
            IndexType::Gin => "gin",
            IndexType::Inverted => "inverted",
            IndexType::HNSW => "hnsw",
            IndexType::IVF => "ivf",
            IndexType::Graph => "graph",
            IndexType::RTree => "rtree",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDef {
    pub name: String,
    pub index_type: IndexType,
    pub table_name: String,
    pub columns: Vec<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
}

impl IndexDef {
    pub fn new(
        name: impl Into<String>,
        index_type: IndexType,
        table_name: impl Into<String>,
        columns: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            index_type,
            table_name: table_name.into(),
            columns,
            parameters: BTreeMap::new(),
        }
    }

    pub fn with_parameter(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.parameters.insert(key.into(), value.into());
        self
    }

    pub fn parameter(&self, key: &str) -> Option<&str> {
        self.parameters.get(key).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips() {
        for kind in [
            IndexType::BTree,
            IndexType::Gin,
            IndexType::Inverted,
            IndexType::IVF,
            IndexType::Graph,
            IndexType::RTree,
        ] {
            assert_eq!(IndexType::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(IndexType::parse("BTREE"), Some(IndexType::BTree));
        assert_eq!(IndexType::parse("HnSw"), Some(IndexType::IVF));
    }
}
