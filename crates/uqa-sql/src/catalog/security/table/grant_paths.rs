//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary privilege checks and admitted catalog validation share the same ordered grant closure.

use super::{acl_grantor, TableAclEntry, TableAclPrivilege};
use crate::SQLError;
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

pub(in crate::catalog::security) fn extend<'a>(
    owner: &'a str,
    acl: Option<&'a [TableAclEntry]>,
    privilege: TableAclPrivilege,
    reachable: &mut impl Paths<'a>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let Some(acl) = acl else {
        return Ok(());
    };
    loop {
        let mut changed = false;
        for entry in acl {
            control.check()?;
            let Some(role) = entry.role.role_name() else {
                continue;
            };
            if entry.grant_options.intersects(privilege.mask())
                && reachable.contains(acl_grantor(entry, owner))
            {
                changed |= reachable.insert(role)?;
            }
        }
        if !changed {
            return Ok(());
        }
    }
}
