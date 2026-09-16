//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field-owned storage and ordinary catalog index references share one evaluated column lifecycle batch.

mod records;

use super::{string, text, Catalog, Family, NativeRecordOwner, NativeSnapshot, Result};
use crate::catalog::RelationIdentity;
use crate::catalog_lifecycle::{columns_json_references, renamed_columns_json};
use crate::mvcc::native::decode_record;
use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{mvcc::VersionError, KeyValueBatch};

impl Catalog {
    pub(in crate::catalog) fn change_native_column(
        &self,
        table: &str,
        from: &str,
        to: Option<&str>,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            snapshot.change_column_indexes(batch, table, from, to)?;
            let Some(owner) = snapshot.table_owner(table)? else {
                return Ok(());
            };
            for family in Family::all() {
                if matches!(
                    family,
                    Family::BtreeIndexes | Family::BtreeIndexEntries | Family::BtreeIndexRepairs
                ) {
                    continue;
                }
                let Some(column) = field_column(family) else {
                    continue;
                };
                if let Some(to) = to {
                    snapshot.rename_field(batch, family, owner, column, from, to)?;
                } else {
                    snapshot.visit_field_keys(family, owner, column, from, None, |key| {
                        batch.delete(key)?;
                        Ok(true)
                    })?;
                }
            }
            crate::btree_index::change_native_column(snapshot, batch, table, from, to)
        })
    }
}

fn field_column(family: Family) -> Option<usize> {
    let layout = family.layout();
    if !layout.columns.contains(&"table_name") {
        return None;
    }
    layout
        .columns
        .iter()
        .position(|name| matches!(*name, "field" | "field_name" | "column_name"))
}

impl NativeSnapshot {
    fn change_column_indexes(
        &self,
        batch: &mut dyn KeyValueBatch,
        table: &str,
        from: &str,
        to: Option<&str>,
    ) -> Result<()> {
        let owner = NativeRecordOwner::Database(self.database);
        self.visit_rows(Family::CatalogIndexes, Some(owner), &[], |row| {
            if RelationIdentity::new(string(row[4])?, string(row[5])?).qualified_name() != table {
                return Ok(());
            }
            let encoded = string(row[6])?;
            if let Some(to) = to {
                if let Some(columns) = renamed_columns_json(&encoded, from, to)? {
                    let mut changed = BudgetedVec::new(self.control.memory());
                    changed.extend_from_slice(row).map_err(VersionError::from)?;
                    changed[6] = text(&columns);
                    self.put_row(batch, Family::CatalogIndexes, owner, &changed)?;
                }
            } else if columns_json_references(&encoded, from)? {
                self.delete_prefix(batch, Family::CatalogIndexes, owner, &[row[0], row[1]])?;
                self.delete_prefix(batch, Family::Relations, owner, &[row[0], row[1]])?;
            }
            Ok(())
        })
    }

    fn rename_field(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        owner: NativeRecordOwner,
        column: usize,
        from: &str,
        to: &str,
    ) -> Result<()> {
        let mut after = BudgetedVec::new(self.control.memory());
        loop {
            let mut key = BudgetedVec::new(self.control.memory());
            self.visit_field_keys(
                family,
                owner,
                column,
                from,
                (!after.is_empty()).then_some(&after[..]),
                |next| {
                    key.extend_from_slice(next).map_err(VersionError::from)?;
                    Ok(false)
                },
            )?;
            if key.is_empty() {
                break;
            }
            // Release the physical visitor before probing the destination of this retained source row.
            let record = self.view.get(&key, &self.control)?;
            if let Some(bytes) = record.as_ref().and_then(|record| record.value()) {
                let (_, mut values) = decode_record(&key, bytes, &self.control)?;
                values[column] = text(to);
                let mut components = BudgetedVec::new(self.control.memory());
                for column in family.layout().identity_columns {
                    components.push(values[*column])?;
                }
                if !self.contains_row(family, owner, &components)? {
                    self.put_row(batch, family, owner, &values)?;
                }
                batch.delete(&key)?;
            }
            after = key;
        }
        Ok(())
    }
}
