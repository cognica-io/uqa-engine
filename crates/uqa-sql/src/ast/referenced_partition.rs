//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The constraints `PostgreSQL` derives from a foreign key on the partitions of the partitioned table it references.

use serde::{Deserialize, Serialize};

use super::ConstraintCatalogIdentity;

/// The constraint `PostgreSQL` derives from a foreign key on one partition of the partitioned table it references, whose `pg_constraint.conparentid` names the constraint it derives from. It names the partition's share of the referenced rows in `pg_constraint`, in errors about those rows and in `DETACH PARTITION`, and keeps the name chosen when the partition joined the referenced table, whatever later happens to the name of the foreign key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferencedPartitionConstraint {
    /// Object identity of the referenced partition.
    pub partition: [u8; 16],
    /// Object identity of the partitioned partition whose derived constraint this one derives from, or `None` when it derives from the foreign key itself, which references the partition's parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<[u8; 16]>,
    pub name: String,
    pub catalog_identity: ConstraintCatalogIdentity,
    /// Whether the referenced partition's share of the referencing rows is known to hold. It follows the foreign key, except that `VALIDATE CONSTRAINT` of an unvalidated foreign key's derived constraint validates that constraint and the ones deriving from it alone.
    pub validated: bool,
}
