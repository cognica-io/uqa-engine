//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Renaming a column follows B-tree parent replacement and leaves named expression namespaces intact.

use super::{
    clear_repair, drop_index, Family, KeyValueBatch, NativeSnapshot, Result, ValueIndexKey,
    ValueRef,
};

pub(crate) fn change_column(
    snapshot: &NativeSnapshot,
    batch: &mut dyn KeyValueBatch,
    table: &str,
    from: &str,
    to: Option<&str>,
) -> Result<()> {
    let Some(owner) = snapshot.table_owner(table)? else {
        return Ok(());
    };
    let source = ValueRef::Text(from.as_bytes());
    if let Some(to) = to {
        let target = ValueRef::Text(to.as_bytes());
        if !snapshot.contains_row(Family::BtreeIndexes, owner, &[target])? {
            for family in [
                Family::BtreeIndexes,
                Family::BtreeIndexEntries,
                Family::BtreeIndexRepairs,
            ] {
                snapshot.visit_rows(family, Some(owner), &[source], |row| match family {
                    Family::BtreeIndexEntries => {
                        snapshot.put_row(batch, family, owner, &[row[0], target, row[2], row[3]])
                    }
                    _ => snapshot.put_row(batch, family, owner, &[row[0], target]),
                })?;
            }
        }
    }
    let field = ValueIndexKey::Column(from.into());
    drop_index(snapshot, batch, table, &field)?;
    clear_repair(snapshot, batch, table, &field)
}
