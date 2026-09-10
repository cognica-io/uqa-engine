//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::RelationIdentity;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConstraintIdentity {
    pub relation: RelationIdentity,
    pub name: String,
    pub object_id: Option<[u8; 16]>,
}

pub fn constraint_identities_match(left: &ConstraintIdentity, right: &ConstraintIdentity) -> bool {
    match (left.object_id, right.object_id) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}
