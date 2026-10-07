//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-scoped pins for exact row addresses; key reservations already carry their complete fixed identity.

use super::super::RowIdentity;
use super::{FileLockCoordinator, RelationIdentityLease};
use uqa_core::CancellationToken;
use uqa_sql::SQLError;

pub(in crate::row_locks) struct RowIdentityLease<'a> {
    identity: RowIdentity,
    relation: Option<RelationIdentityLease<'a>>,
}

impl RowIdentityLease<'_> {
    pub(in crate::row_locks) fn identity(&self) -> RowIdentity {
        self.identity
    }
    pub(in crate::row_locks) fn retain(&mut self) {
        if let Some(relation) = &mut self.relation {
            relation.retain();
        }
    }
}

impl FileLockCoordinator {
    pub(in crate::row_locks) fn pin_row<'a>(
        &'a self,
        relation: &'a [u8],
        doc_id: u64,
        cancel: &CancellationToken,
    ) -> Result<RowIdentityLease<'a>, SQLError> {
        cancel.check()?;
        if let Some(identity) = RowIdentity::key(relation, doc_id) {
            return Ok(RowIdentityLease {
                identity,
                relation: None,
            });
        }
        let relation = self.pin_relation(relation, cancel)?;
        let identity = RowIdentity::Relation {
            generation: relation.generation(),
            doc_id,
        };
        Ok(RowIdentityLease {
            identity,
            relation: Some(relation),
        })
    }
}

#[cfg(test)]
mod tests;
