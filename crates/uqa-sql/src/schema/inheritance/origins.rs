//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Track whether NOT NULL and CHECK declarations are local after a hierarchy edge changes.
use crate::ast::{ColumnDef, TableCheck, TableHierarchy};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub struct InheritanceOriginChange {
    removed_parent: bool,
    attached_partition: bool,
}
impl InheritanceOriginChange {
    pub fn between(previous: &TableHierarchy, next: &TableHierarchy) -> Option<Self> {
        let removed_parent = previous
            .parents
            .iter()
            .any(|parent| !next.parents.contains(parent));
        let attached_partition = !previous.is_partition() && next.is_partition();
        (removed_parent || attached_partition).then_some(Self {
            removed_parent,
            attached_partition,
        })
    }
    pub fn update_not_null(self, columns: &mut [ColumnDef], inherited: &BTreeSet<String>) {
        for column in columns.iter_mut().filter(|column| column.not_null) {
            if self.attached_partition && inherited.contains(&column.name) {
                column.not_null_is_local = false;
            } else if self.removed_parent && !inherited.contains(&column.name) {
                column.not_null_is_local = true;
            }
        }
    }
    pub fn update_checks(
        self,
        columns: &mut [ColumnDef],
        checks: &mut [TableCheck],
        inherited: &BTreeSet<String>,
    ) {
        let update = |name: Option<&String>, local: &mut bool| {
            let supplied = name.is_some_and(|name| inherited.contains(name));
            if self.attached_partition && supplied {
                *local = false;
            } else if self.removed_parent && !supplied {
                *local = true;
            }
        };
        for column in columns.iter_mut().filter(|column| column.check.is_some()) {
            update(column.check_name.as_ref(), &mut column.check_is_local);
        }
        for check in checks {
            update(check.name.as_ref(), &mut check.is_local);
        }
    }
}
