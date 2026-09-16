//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column statistics retain the table's exact storage name and share its committed/private record view.

use super::{
    optional_text, string, text, Catalog, Family, NativeRecordOwner, NativeSnapshot, Result,
    SQLiteError,
};
use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{ColumnStatsInput, ColumnStatsRow, KeyValueBatch};

impl NativeSnapshot {
    fn put_column_stats(
        &self,
        batch: &mut dyn KeyValueBatch,
        owner: NativeRecordOwner,
        row: ColumnStatsInput<'_>,
    ) -> Result<()> {
        self.put_row(
            batch,
            Family::ColumnStats,
            owner,
            &[
                text(row.table_name),
                text(row.column_name),
                ValueRef::Integer(row.distinct_count),
                ValueRef::Integer(row.null_count),
                optional_text(row.min_value),
                optional_text(row.max_value),
                ValueRef::Integer(row.row_count),
                text(row.histogram_json),
                text(row.mcv_values_json),
                text(row.mcv_frequencies_json),
            ],
        )
    }
}

impl Catalog {
    pub(in crate::catalog) fn save_native_column_stats(
        &self,
        row: ColumnStatsInput<'_>,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            let owner = snapshot.ensure_table_owner(row.table_name, batch)?;
            snapshot.put_column_stats(batch, owner, row)
        })
    }

    pub(in crate::catalog) fn replace_native_column_stats(
        &self,
        table: &str,
        rows: &[ColumnStatsInput<'_>],
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            let mut names = BudgetedVec::new(snapshot.control.memory());
            for row in rows {
                names.push(row.column_name)?;
            }
            names.sort_unstable();
            if names.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(SQLiteError::StorageBackend(
                    "duplicate column in statistics replacement".into(),
                ));
            }
            let owner = match snapshot.table_owner(table)? {
                Some(owner) => owner,
                None if rows.is_empty() => return Ok(()),
                None => snapshot.ensure_table_owner(table, batch)?,
            };
            snapshot.delete_prefix(batch, Family::ColumnStats, owner, &[])?;
            for row in rows {
                snapshot.put_column_stats(batch, owner, *row)?;
            }
            Ok(())
        })
    }

    pub(in crate::catalog) fn load_native_column_stats(
        &self,
        table: &str,
    ) -> Result<Option<Vec<ColumnStatsRow>>> {
        self.read_native(|snapshot| {
            let mut rows = Vec::new();
            if let Some(owner) = snapshot.table_owner(table)? {
                snapshot.visit_rows(Family::ColumnStats, Some(owner), &[], |row| {
                    let integer = |value: ValueRef<'_>| {
                        value.as_i64().map_err(|_| {
                            SQLiteError::StorageBackend(
                                "native statistics count must be integer".into(),
                            )
                        })
                    };
                    let optional = |value| {
                        if value == ValueRef::Null {
                            Ok(None)
                        } else {
                            string(value).map(Some)
                        }
                    };
                    rows.push(ColumnStatsRow {
                        column_name: string(row[1])?,
                        distinct_count: integer(row[2])?,
                        null_count: integer(row[3])?,
                        min_value: optional(row[4])?,
                        max_value: optional(row[5])?,
                        row_count: integer(row[6])?,
                        histogram_json: string(row[7])?,
                        mcv_values_json: string(row[8])?,
                        mcv_frequencies_json: string(row[9])?,
                    });
                    Ok(())
                })?;
            }
            Ok(rows)
        })
    }

    pub(in crate::catalog) fn delete_native_column_stats(&self, table: &str) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            if let Some(owner) = snapshot.table_owner(table)? {
                snapshot.delete_prefix(batch, Family::ColumnStats, owner, &[])?;
            }
            Ok(())
        })
    }
}
