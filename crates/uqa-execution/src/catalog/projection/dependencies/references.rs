//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The objects one catalog definition references, collected as `find_expr_references_walker` collects them into an `ObjectAddresses` list.

use std::cmp::Ordering;
use uqa_sql::catalog::dependencies::{ObjectAddress, PROCEDURE_CLASS, RELATION_CLASS, TYPE_CLASS};

#[derive(Debug, Clone, Default)]
pub(super) struct References {
    addresses: Vec<ObjectAddress>,
}

impl References {
    pub(super) fn add(&mut self, address: ObjectAddress) {
        self.addresses.push(address);
    }

    pub(super) fn add_type(&mut self, oid: u32) {
        self.add(ObjectAddress::whole(TYPE_CLASS, oid));
    }

    pub(super) fn add_relation(&mut self, oid: u32) {
        self.add(ObjectAddress::whole(RELATION_CLASS, oid));
    }

    pub(super) fn add_column(&mut self, relation: u32, column_number: i32) {
        self.add(ObjectAddress::column(relation, column_number));
    }

    pub(super) fn add_routine(&mut self, oid: u32) {
        self.add(ObjectAddress::whole(PROCEDURE_CLASS, oid));
    }

    pub(super) fn is_empty(&self) -> bool {
        self.addresses.is_empty()
    }

    /// `eliminate_duplicate_dependencies`: the references in `object_address_comparator` order without repeats. A whole relation that is referenced together with its columns gives way to the columns.
    pub(super) fn deduplicated(mut self) -> Vec<ObjectAddress> {
        self.addresses.sort_by(address_order);
        let mut output: Vec<ObjectAddress> = Vec::with_capacity(self.addresses.len());
        for address in self.addresses {
            if let Some(prior) = output.last_mut() {
                if prior.class_id == address.class_id && prior.object_id == address.object_id {
                    if prior.sub_id == address.sub_id {
                        continue;
                    }
                    if prior.sub_id == 0 {
                        prior.sub_id = address.sub_id;
                        continue;
                    }
                }
            }
            output.push(address);
        }
        output
    }
}

/// `object_address_comparator`: descending OID, then ascending catalog, then ascending column with the whole object first.
fn address_order(left: &ObjectAddress, right: &ObjectAddress) -> Ordering {
    right
        .object_id
        .cmp(&left.object_id)
        .then(left.class_id.cmp(&right.class_id))
        .then(
            left.sub_id
                .cast_unsigned()
                .cmp(&right.sub_id.cast_unsigned()),
        )
}

#[cfg(test)]
mod tests;
