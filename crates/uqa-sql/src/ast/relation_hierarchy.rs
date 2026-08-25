//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inheritance and declarative-partitioning catalog nodes.

use serde::{Deserialize, Serialize};

use super::Expr;

/// Metadata shared by ordinary inheritance and declarative partitioning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TableHierarchy {
    /// Direct parents in declaration order. Names are parse-time identities in
    /// the statement AST and canonical catalog identities after registration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    /// Partition key owned by a partitioned relation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_spec: Option<PartitionSpec>,
    /// Bound owned by a child declared with `PARTITION OF`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_bound: Option<PartitionBound>,
}

impl TableHierarchy {
    #[must_use]
    pub const fn is_partition(&self) -> bool {
        self.partition_bound.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartitionStrategy {
    List,
    Range,
    Hash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionSpec {
    pub strategy: PartitionStrategy,
    pub keys: Vec<Expr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PartitionBound {
    Default,
    List(Vec<Expr>),
    Range {
        lower: Vec<PartitionRangeDatum>,
        upper: Vec<PartitionRangeDatum>,
    },
    Hash {
        modulus: i32,
        remainder: i32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PartitionRangeDatum {
    MinValue,
    Value(Expr),
    MaxValue,
}
