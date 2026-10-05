//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A catalog object as `PostgreSQL`'s `ObjectAddress` names it.

use std::cmp::Ordering;

/// `pg_class`: relations, and with a column number, their columns.
pub const RELATION_CLASS: u32 = 1259;
/// `pg_type`.
pub const TYPE_CLASS: u32 = 1247;
/// `pg_proc`.
pub const PROCEDURE_CLASS: u32 = 1255;
/// `pg_constraint`.
pub const CONSTRAINT_CLASS: u32 = 2606;
/// `pg_attrdef`: column defaults and generation expressions.
pub const ATTRIBUTE_DEFAULT_CLASS: u32 = 2604;
/// `pg_rewrite`.
pub const REWRITE_CLASS: u32 = 2618;
/// `pg_trigger`.
pub const TRIGGER_CLASS: u32 = 2620;
/// `pg_namespace`.
pub const NAMESPACE_CLASS: u32 = 2615;
/// `pg_language`.
pub const LANGUAGE_CLASS: u32 = 2612;
/// `pg_authid`: roles, which every database shares.
pub const ROLE_CLASS: u32 = 1260;
/// `pg_auth_members`: role memberships, which every database shares.
pub const ROLE_MEMBERSHIP_CLASS: u32 = 1261;
/// `pg_database`, which every database shares.
pub const DATABASE_CLASS: u32 = 1262;

/// The catalog holding an object's row, the row's OID, and for a column of a relation its column number; zero names the whole object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectAddress {
    pub class_id: u32,
    pub object_id: u32,
    pub sub_id: i32,
}

impl ObjectAddress {
    #[must_use]
    pub const fn whole(class_id: u32, object_id: u32) -> Self {
        Self {
            class_id,
            object_id,
            sub_id: 0,
        }
    }

    /// A column of a relation.
    #[must_use]
    pub const fn column(relation: u32, column_number: i32) -> Self {
        Self {
            class_id: RELATION_CLASS,
            object_id: relation,
            sub_id: column_number,
        }
    }

    /// The whole object this address, or one of its columns, belongs to.
    #[must_use]
    pub const fn whole_object(self) -> Self {
        Self::whole(self.class_id, self.object_id)
    }

    pub(super) const fn same_object(self, other: Self) -> bool {
        self.class_id == other.class_id && self.object_id == other.object_id
    }

    /// `object_address_comparator`: newer objects, by descending OID, come first, then catalog and column in ascending order with the whole object before its columns.
    pub(super) fn deletion_order(&self, other: &Self) -> Ordering {
        other
            .object_id
            .cmp(&self.object_id)
            .then(self.class_id.cmp(&other.class_id))
            .then(
                self.sub_id
                    .cast_unsigned()
                    .cmp(&other.sub_id.cast_unsigned()),
            )
    }
}
