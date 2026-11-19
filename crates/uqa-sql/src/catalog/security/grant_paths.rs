//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered grant closure is shared by relation and sequence privilege carriers.

use crate::{catalog::work::Work, SQLError};
use std::collections::BTreeSet;
use uqa_core::memory::{ProductionControl, ProductionMap};

pub(in crate::catalog::security) trait Paths<'a> {
    fn contains(&self, role: &str) -> bool;
    fn insert(&mut self, role: &'a str) -> Result<bool, SQLError>;
}

impl Paths<'_> for BTreeSet<String> {
    fn contains(&self, role: &str) -> bool {
        self.contains(role)
    }

    fn insert(&mut self, role: &str) -> Result<bool, SQLError> {
        Ok(self.insert(role.to_owned()))
    }
}

impl<'a> Paths<'a> for ProductionMap<'_, &'a str, ()> {
    fn contains(&self, role: &str) -> bool {
        self.get(role).is_some()
    }

    fn insert(&mut self, role: &'a str) -> Result<bool, SQLError> {
        Ok(self.insert_copy(role, ())?.is_none())
    }
}

pub(super) trait Grant: Sync {
    type Privilege: Copy + Send;
    fn recipient(&self) -> Option<&str>;
    fn grantor<'a>(&'a self, owner: &'a str) -> &'a str;
    fn grants_option(&self, privilege: Self::Privilege) -> bool;
}

pub(super) async fn extend_from<'a, E: Grant>(
    owner: &'a str,
    acl: Option<&'a [E]>,
    privilege: E::Privilege,
    reachable: &mut impl Paths<'a>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    let Some(acl) = acl else {
        return Ok(());
    };
    loop {
        let mut changed = false;
        for entry in acl {
            work.checkpoint().await?;
            control.check()?;
            let Some(role) = entry.recipient() else {
                continue;
            };
            if entry.grants_option(privilege) && reachable.contains(entry.grantor(owner)) {
                changed |= reachable.insert(role)?;
            }
        }
        if !changed {
            return Ok(());
        }
    }
}
