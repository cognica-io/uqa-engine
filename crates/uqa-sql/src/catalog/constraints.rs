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

/// Reconcile a retained constraint with a renamed row before considering a renamed relation. A removed row on a surviving relation must not bind to another partition copy.
pub fn find_live_constraint_identity<'a>(
    live: &'a [ConstraintIdentity],
    live_relations: &std::collections::BTreeSet<RelationIdentity>,
    identity: &ConstraintIdentity,
) -> Option<&'a ConstraintIdentity> {
    live.iter()
        .find(|current| *current == identity)
        .or_else(|| {
            live.iter().find(|current| {
                current.relation == identity.relation
                    && constraint_identities_match(identity, current)
            })
        })
        .or_else(|| {
            if live_relations.contains(&identity.relation) {
                return None;
            }
            live.iter()
                .find(|current| constraint_identities_match(identity, current))
        })
}

pub fn foreign_key_identity(
    table: &str,
    foreign_key: &crate::ast::ForeignKey,
) -> Result<ConstraintIdentity, crate::SQLError> {
    let relation = RelationIdentity::from_legacy_name(table).map_err(|error| {
        crate::SQLError::Internal(format!(
            "decode foreign-key relation identity '{table}': {error}"
        ))
    })?;
    let name = foreign_key.name.clone().ok_or_else(|| {
        crate::SQLError::Internal(format!(
            "foreign key on '{table}' has no materialized constraint name"
        ))
    })?;
    let object_id = foreign_key.object_id.ok_or_else(|| {
        crate::SQLError::Internal(format!(
            "foreign key '{name}' on '{table}' has no materialized object identity"
        ))
    })?;
    Ok(ConstraintIdentity {
        relation,
        name,
        object_id: Some(object_id),
    })
}

#[cfg(test)]
mod tests;
