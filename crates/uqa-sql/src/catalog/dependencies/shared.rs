//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One `pg_shdepend` row, and the detail `DROP ROLE` reports from the rows that name a role, as `checkSharedDependencies` builds it.

use super::ObjectAddress;
use std::cmp::Ordering;
use std::fmt::Write as _;

/// `pg_shdepend.deptype`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SharedDependencyKind {
    /// `o`: the referenced role owns the object.
    Owner,
    /// `a`: the object's privileges name the referenced role as a grantee or grantor.
    Acl,
    /// `i`: the object's initial privileges name the referenced role.
    InitialAcl,
    /// `r`: a row-level security policy applies to the referenced role.
    Policy,
    /// `t`: the object's storage is in the referenced tablespace.
    Tablespace,
}

impl SharedDependencyKind {
    /// The `pg_shdepend.deptype` letter.
    #[must_use]
    pub const fn code(self) -> char {
        match self {
            Self::Owner => 'o',
            Self::Acl => 'a',
            Self::InitialAcl => 'i',
            Self::Policy => 'r',
            Self::Tablespace => 't',
        }
    }

    fn describe(self, object: &str) -> String {
        match self {
            Self::Owner => format!("owner of {object}"),
            Self::Acl => format!("privileges for {object}"),
            Self::InitialAcl => format!("initial privileges for {object}"),
            Self::Policy => format!("target of {object}"),
            Self::Tablespace => format!("tablespace for {object}"),
        }
    }
}

/// A dependency of an object on a role or tablespace, which every database shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SharedDependency {
    /// `dbid`: the database holding the dependent object, or zero for an object that every database shares.
    pub database: u32,
    pub dependent: ObjectAddress,
    pub referenced: ObjectAddress,
    pub kind: SharedDependencyKind,
}

/// `MAX_REPORTED_DEPS`: the detail lists at most this many objects.
const MAX_REPORTED_DEPENDENTS: usize = 100;

/// `checkSharedDependencies`: the detail listing what depends on `referenced`, by ascending OID, catalog, column and dependency type; `None` when nothing that can be described depends on it. Every row must belong to the current database or be shared.
pub fn shared_dependency_detail<'a>(
    dependencies: impl IntoIterator<Item = &'a SharedDependency>,
    referenced: ObjectAddress,
    describe: super::Describe<'_>,
) -> Result<Option<String>, crate::SQLError> {
    let mut dependents = dependencies
        .into_iter()
        .filter(|dependency| dependency.referenced == referenced)
        .map(|dependency| (dependency.dependent, dependency.kind))
        .collect::<Vec<_>>();
    dependents.sort_by(|(left, left_kind), (right, right_kind)| {
        shared_dependency_order(left, right).then(left_kind.code().cmp(&right_kind.code()))
    });
    let mut lines = Vec::new();
    let mut unreported = 0_usize;
    for (index, (object, kind)) in dependents.into_iter().enumerate() {
        if index >= MAX_REPORTED_DEPENDENTS {
            unreported += 1;
            continue;
        }
        // An object that cannot be described is skipped but still counts toward the reported limit.
        if let Some(description) = describe(object)? {
            lines.push(kind.describe(&description));
        }
    }
    if lines.is_empty() {
        return Ok(None);
    }
    let mut detail = lines.join("\n");
    if unreported > 0 {
        write!(
            detail,
            "\nand {unreported} other object{} (see server log for list)",
            if unreported == 1 { "" } else { "s" }
        )
        .expect("writing to a String cannot fail");
    }
    Ok(Some(detail))
}

/// `shared_dependency_comparator`: ascending OID, then catalog, then column with the whole object first.
fn shared_dependency_order(left: &ObjectAddress, right: &ObjectAddress) -> Ordering {
    left.object_id
        .cmp(&right.object_id)
        .then(left.class_id.cmp(&right.class_id))
        .then(
            left.sub_id
                .cast_unsigned()
                .cmp(&right.sub_id.cast_unsigned()),
        )
}

#[cfg(test)]
mod tests;
