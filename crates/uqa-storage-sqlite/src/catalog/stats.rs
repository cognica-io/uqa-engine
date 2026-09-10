//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted column statistics.

use super::{params, Catalog, ColumnStatsInput, ColumnStatsRow, Result, SQLiteError};

impl Catalog {
    /// Persist a per-column ANALYZE summary so the planner still has
    /// cardinality / range estimates after a restart. `min_value` and
    /// `max_value` are stored as strings (JSON when the value isn't
    /// natively textual) so the column type is irrelevant.
    pub fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _column_stats
                    (table_name, column_name, distinct_count, null_count,
                     min_value, max_value, row_count,
                     histogram, mcv_values, mcv_frequencies)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    stats.table_name,
                    stats.column_name,
                    stats.distinct_count,
                    stats.null_count,
                    stats.min_value,
                    stats.max_value,
                    stats.row_count,
                    stats.histogram_json,
                    stats.mcv_values_json,
                    stats.mcv_frequencies_json,
                ],
            )?;
            Ok(())
        })
    }

    pub fn replace_column_stats(
        &self,
        table_name: &str,
        stats: &[ColumnStatsInput<'_>],
    ) -> Result<()> {
        if let Some(row) = stats.iter().find(|row| row.table_name != table_name) {
            return Err(SQLiteError::StorageBackend(format!(
                "column stats row for table `{}` cannot be stored in snapshot `{table_name}`",
                row.table_name
            )));
        }
        self.conn.with_mut(|connection| {
            let transaction = connection.savepoint()?;
            transaction.execute(
                "DELETE FROM _column_stats WHERE table_name = ?1",
                params![table_name],
            )?;
            {
                let mut statement = transaction.prepare(
                    "INSERT INTO _column_stats
                        (table_name, column_name, distinct_count, null_count,
                         min_value, max_value, row_count,
                         histogram, mcv_values, mcv_frequencies)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )?;
                for row in stats {
                    statement.execute(params![
                        row.table_name,
                        row.column_name,
                        row.distinct_count,
                        row.null_count,
                        row.min_value,
                        row.max_value,
                        row.row_count,
                        row.histogram_json,
                        row.mcv_values_json,
                        row.mcv_frequencies_json,
                    ])?;
                }
            }
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn load_column_stats(&self, table_name: &str) -> Result<Vec<ColumnStatsRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT column_name, distinct_count, null_count,
                        min_value, max_value, row_count,
                        histogram, mcv_values, mcv_frequencies
                   FROM _column_stats
                  WHERE table_name = ?1
                  ORDER BY column_name",
            )?;
            let rows = stmt.query_map(params![table_name], |r| {
                Ok(ColumnStatsRow {
                    column_name: r.get::<_, String>(0)?,
                    distinct_count: r.get::<_, i64>(1)?,
                    null_count: r.get::<_, i64>(2)?,
                    min_value: r.get::<_, Option<String>>(3)?,
                    max_value: r.get::<_, Option<String>>(4)?,
                    row_count: r.get::<_, i64>(5)?,
                    histogram_json: r.get::<_, String>(6)?,
                    mcv_values_json: r.get::<_, String>(7)?,
                    mcv_frequencies_json: r.get::<_, String>(8)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn delete_column_stats(&self, table_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _column_stats WHERE table_name = ?1",
                params![table_name],
            )?;
            Ok(())
        })
    }
}
