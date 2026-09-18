//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Added role dependencies are computed for each independently stored ACL.

use super::{TableAclEntry, TableSecurity};
use std::collections::BTreeSet;

fn acl_roles<'a>(acl: &'a [TableAclEntry], owner: &'a str) -> BTreeSet<&'a str> {
    acl.iter()
        .flat_map(|entry| {
            [
                entry.role.as_str(),
                entry.grantor.as_deref().unwrap_or(owner),
            ]
        })
        .filter(|role| *role != "PUBLIC" && *role != owner)
        .collect()
}

/// An existing dependency in another column or the relation ACL does not replace the dependency of this ACL. Ownership already protects the owner; PUBLIC has no role object.
pub fn added_table_acl_roles(
    before: &TableSecurity,
    after: &TableSecurity,
    added: &mut BTreeSet<String>,
) {
    let mut compare = |old: &[TableAclEntry], new: &[TableAclEntry]| {
        let old = acl_roles(old, &before.role_owner);
        let new = acl_roles(new, &after.role_owner);
        added.extend(new.difference(&old).map(|role| (*role).to_owned()));
    };
    compare(
        before.acl.as_deref().unwrap_or_default(),
        after.acl.as_deref().unwrap_or_default(),
    );
    for (column, acl) in &after.column_acls {
        compare(
            before.column_acls.get(column).map_or(&[], Vec::as_slice),
            acl,
        );
    }
}

#[cfg(test)]
mod tests;
