//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement's staged membership deltas retain their original catalog versions through waits.

use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{
    catalog::roles::{memberships::command::MembershipUpdate, RoleMembership, RoleMembershipKey},
    SQLError,
};

#[derive(Default)]
pub(super) struct MembershipOverlay {
    changes: BTreeMap<RoleMembershipKey, (Option<RoleMembership>, Option<RoleMembership>)>,
}

impl MembershipOverlay {
    pub(super) fn apply(
        &self,
        current: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<BTreeMap<RoleMembershipKey, RoleMembership>, SQLError> {
        let mut next = current.clone();
        for (key, (before, after)) in &self.changes {
            if current.get(key) != before.as_ref() {
                return Err(SQLError::Internal(
                    if current.contains_key(key) {
                        "tuple concurrently updated"
                    } else {
                        "tuple concurrently deleted"
                    }
                    .into(),
                ));
            }
            if let Some(after) = after {
                next.insert(*key, after.clone());
            } else {
                next.remove(key);
            }
        }
        Ok(next)
    }

    pub(super) fn insert(&mut self, membership: RoleMembership) {
        let change = self.changes.entry(membership.key()).or_insert((None, None));
        change.1 = Some(membership);
    }

    pub(super) fn update(&mut self, update: MembershipUpdate) {
        let key = update.before.key();
        let change = self
            .changes
            .entry(key)
            .or_insert((Some(update.before), None));
        change.1 = update.after;
        if change.0 == change.1 {
            self.changes.remove(&key);
        }
    }

    pub(super) fn oids(&self) -> BTreeSet<i64> {
        self.changes
            .values()
            .filter_map(|(_, after)| after.as_ref().map(|membership| membership.oid))
            .collect()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}
