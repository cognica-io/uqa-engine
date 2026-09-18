//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Added role dependencies are computed for each independently stored ACL.

use super::{database::DatabaseAclEntry, TableAclEntry, TableSecurity};
use std::collections::BTreeSet;
use uqa_core::{catalog_schema::SchemaAclEntry, catalog_sequence::SequenceAclEntry};

pub trait AclRoleReferences {
    fn role_references(&self) -> (Option<&str>, Option<&str>);
}

impl AclRoleReferences for TableAclEntry {
    fn role_references(&self) -> (Option<&str>, Option<&str>) {
        (self.role.role_name(), self.grantor.as_deref())
    }
}

impl AclRoleReferences for SchemaAclEntry {
    fn role_references(&self) -> (Option<&str>, Option<&str>) {
        (self.role.role_name(), self.grantor.as_deref())
    }
}

impl AclRoleReferences for SequenceAclEntry {
    fn role_references(&self) -> (Option<&str>, Option<&str>) {
        (self.role.role_name(), self.grantor.as_deref())
    }
}

impl AclRoleReferences for DatabaseAclEntry {
    fn role_references(&self) -> (Option<&str>, Option<&str>) {
        (self.role.role_name(), self.grantor.as_deref())
    }
}

impl AclRoleReferences for crate::ast::RoutineAclEntry {
    fn role_references(&self) -> (Option<&str>, Option<&str>) {
        (self.role.role_name(), self.grantor.as_deref())
    }
}

fn acl_roles<'a, T: AclRoleReferences>(acl: &'a [T], owner: &'a str) -> BTreeSet<&'a str> {
    acl.iter()
        .flat_map(|entry| {
            let (role, grantor) = entry.role_references();
            [role, Some(grantor.unwrap_or(owner))].into_iter().flatten()
        })
        .filter(|role| *role != owner)
        .collect()
}

pub fn added_acl_roles<T: AclRoleReferences>(
    before: &[T],
    before_owner: &str,
    after: &[T],
    after_owner: &str,
    added: &mut BTreeSet<String>,
) {
    let old = acl_roles(before, before_owner);
    let new = acl_roles(after, after_owner);
    added.extend(new.difference(&old).map(|role| (*role).to_owned()));
}

/// An existing dependency in another column or the relation ACL does not replace the dependency of this ACL. Ownership already protects the owner; PUBLIC has no role object.
pub fn added_table_acl_roles(
    before: &TableSecurity,
    after: &TableSecurity,
    added: &mut BTreeSet<String>,
) {
    let mut compare = |old: &[TableAclEntry], new: &[TableAclEntry]| {
        added_acl_roles(old, &before.role_owner, new, &after.role_owner, added);
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
