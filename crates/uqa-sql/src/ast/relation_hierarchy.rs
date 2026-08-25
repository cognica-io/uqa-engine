//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inheritance and declarative-partitioning catalog nodes.

use serde::{Deserialize, Serialize};

use super::{AutoIncrement, Expr, ForeignKey, TableKeyConstraint};

/// Metadata shared by ordinary inheritance and declarative partitioning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TableHierarchy {
    /// Direct parents in declaration order. Names are parse-time identities in the statement AST and canonical catalog identities after registration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    /// Durable `pg_inherits.inhseqno` values aligned with `parents`.
    /// Catalogs written before ALTER inheritance support leave this empty and
    /// therefore use the declaration-order sequence `1..=parents.len()`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_sequence_numbers: Vec<i32>,
    /// Partition key owned by a partitioned relation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_spec: Option<PartitionSpec>,
    /// Bound owned by a child declared with `PARTITION OF`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_bound: Option<PartitionBound>,
    /// Columns declared by this relation before inherited columns were merged into its stored row type. `PostgreSQL` exposes this provenance through `pg_attribute.attislocal`; keeping it in durable hierarchy metadata distinguishes an explicitly redeclared inherited column from a purely inherited one after reopen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_columns: Vec<String>,
    /// Original local sequence metadata hidden while an attached partition uses a parent's identity generator. `PostgreSQL` keeps a pre-existing SERIAL default and restores its behavior after DETACH.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partition_identity_overrides: Vec<PartitionIdentityOverride>,
    /// Key constraints copied from a partitioned parent while this relation is attached. The exact copies are retained so DETACH removes only inherited entries and preserves equivalent constraints declared locally before ATTACH.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partition_inherited_key_constraints: Vec<TableKeyConstraint>,
    /// Foreign keys copied from a partitioned parent while this relation is attached. The exact copies are retained so DETACH removes only inherited entries and preserves equivalent constraints declared locally before ATTACH.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partition_inherited_foreign_keys: Vec<ForeignKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionIdentityOverride {
    pub column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<AutoIncrement>,
}

impl TableHierarchy {
    #[must_use]
    pub const fn is_partition(&self) -> bool {
        self.partition_bound.is_some()
    }

    #[must_use]
    pub fn parent_sequence_number(&self, index: usize) -> i32 {
        self.parent_sequence_numbers
            .get(index)
            .copied()
            .unwrap_or_else(|| i32::try_from(index + 1).unwrap_or(i32::MAX))
    }

    #[must_use]
    pub fn next_parent_sequence_number(&self) -> i32 {
        self.parents
            .iter()
            .enumerate()
            .map(|(index, _)| self.parent_sequence_number(index))
            .max()
            .unwrap_or(0)
            .saturating_add(1)
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
