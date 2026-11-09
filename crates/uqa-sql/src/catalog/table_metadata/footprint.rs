//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exhaustive native payload accounting; publication visits no expression or external value source.

use crate::ast::{ForeignKey, ReferencedPartitionConstraint, TableKeyConstraint};
use crate::catalog::relation_attributes::DroppedAttribute;

pub trait Payload {
    fn payload_bytes(&self) -> Option<usize>;
}

pub(super) fn retained<T: Payload>(value: &T) -> Option<usize> {
    size_of::<T>().checked_add(value.payload_bytes()?)
}

impl Payload for String {
    fn payload_bytes(&self) -> Option<usize> {
        Some(self.capacity())
    }
}

impl<T: Payload> Payload for Option<T> {
    fn payload_bytes(&self) -> Option<usize> {
        self.as_ref().map_or(Some(0), Payload::payload_bytes)
    }
}

impl<T: Payload> Payload for Vec<T> {
    fn payload_bytes(&self) -> Option<usize> {
        self.iter().try_fold(
            self.capacity().checked_mul(size_of::<T>())?,
            |bytes, value| bytes.checked_add(value.payload_bytes()?),
        )
    }
}

fn sum(values: &[Option<usize>]) -> Option<usize> {
    values
        .iter()
        .try_fold(0usize, |bytes, value| bytes.checked_add((*value)?))
}

impl Payload for TableKeyConstraint {
    fn payload_bytes(&self) -> Option<usize> {
        let Self {
            catalog_identity: _,
            index_identity: _,
            name,
            kind: _,
            columns,
            included_columns,
            nulls_not_distinct: _,
            without_overlaps: _,
        } = self;
        sum(&[
            name.payload_bytes(),
            columns.payload_bytes(),
            included_columns.payload_bytes(),
        ])
    }
}

impl Payload for ForeignKey {
    fn payload_bytes(&self) -> Option<usize> {
        let Self {
            referenced_index: _,
            referenced_key,
            name,
            object_id: _,
            catalog_identity: _,
            local_columns,
            ref_table,
            ref_columns,
            on_update: _,
            on_delete: _,
            on_delete_set_columns,
            match_type: _,
            enforced: _,
            validated: _,
            deferrable: _,
            initially_deferred: _,
            period: _,
            referenced_partitions,
        } = self;
        sum(&[
            referenced_key.payload_bytes(),
            name.payload_bytes(),
            local_columns.payload_bytes(),
            ref_table.payload_bytes(),
            ref_columns.payload_bytes(),
            on_delete_set_columns.payload_bytes(),
            referenced_partitions.payload_bytes(),
        ])
    }
}

impl Payload for ReferencedPartitionConstraint {
    fn payload_bytes(&self) -> Option<usize> {
        let Self {
            partition: _,
            parent: _,
            name,
            catalog_identity: _,
            validated: _,
        } = self;
        name.payload_bytes()
    }
}

impl Payload for DroppedAttribute {
    fn payload_bytes(&self) -> Option<usize> {
        let Self {
            number: _,
            type_length: _,
            type_modifier: _,
            dimensions: _,
            by_value: _,
            alignment,
            storage,
            collation: _,
            identity,
            is_local: _,
            inheritance_count: _,
        } = self;
        sum(&[
            alignment.payload_bytes(),
            storage.payload_bytes(),
            identity.payload_bytes(),
        ])
    }
}

/// Nested admitted map nodes retain their own leases inside the native value. Include their actual footprint as well as the shared payload header; an outer reservation alone need not own those nodes.
pub(super) fn admitted<T: Payload>(value: &uqa_core::memory::Budgeted<T>) -> Option<usize> {
    retained(&**value)?
        .checked_add(size_of::<uqa_core::memory::MemoryReservation>())
        .map(|bytes| bytes.max(value.reserved_bytes()))
}

impl Payload for crate::catalog::security::BoundTableSecurity {
    fn payload_bytes(&self) -> Option<usize> {
        let Self {
            role_owner: _,
            acl,
            column_acls,
            acl_revisions,
        } = self;
        let acl_bytes =
            |entries: &Vec<crate::catalog::security::table_binding::BoundTableAclEntry>| {
                entries.capacity().checked_mul(size_of::<
                    crate::catalog::security::table_binding::BoundTableAclEntry,
                >())
            };
        let relation = acl.as_ref().map_or(Some(0), acl_bytes)?;
        let columns =
            column_acls
                .iter()
                .try_fold(column_acls.allocated_bytes(), |bytes, (name, acl)| {
                    bytes
                        .checked_add(name.capacity())?
                        .checked_add(acl_bytes(acl)?)
                })?;
        let revisions = acl_revisions
            .columns
            .keys()
            .try_fold(acl_revisions.columns.allocated_bytes(), |bytes, name| {
                bytes.checked_add(name.capacity())
            })?;
        sum(&[Some(relation), Some(columns), Some(revisions)])
    }
}
