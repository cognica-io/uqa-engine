//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain existing field-specific auxiliary rows on the same physical read boundary.

use super::{
    records, BudgetedString, KeyValueReadVisitor, PhysicalRead, Projection, SQLiteError,
    StorageBackendResult, ValueRef,
};
use crate::read_control::reserve_bindings;

impl PhysicalRead<'_> {
    pub(super) fn accelerators(
        &self,
        projection: Projection,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let _binding = reserve_bindings(self.control, &[self.table.as_bytes()])?;
        let mut statement = self
            .connection
            .prepare("SELECT field FROM _occurrence_fields WHERE table_name = ?1 ORDER BY field")
            .map_err(SQLiteError::from)?;
        let mut fields = statement.query([self.table]).map_err(SQLiteError::from)?;
        while let Some(row) = fields.next().map_err(SQLiteError::from)? {
            self.control.check()?;
            let field = row
                .get_ref(0)
                .map_err(SQLiteError::from)?
                .as_str()
                .map_err(|_| records::invalid("invalid occurrence field"))?;
            let mut name = BudgetedString::new(self.control.memory());
            name.push_str(if projection == Projection::Skip {
                "_skip_"
            } else {
                "_blockmax_"
            })?;
            name.push_str(self.table)?;
            name.push('_')?;
            name.push_str(field)?;
            let _name = reserve_bindings(self.control, &[name.as_bytes()])?;
            if super::super::table_exists(self.connection, &name).map_err(SQLiteError::from)? {
                self.accelerator_rows(&name, field, projection, prefix, visit)?;
            }
        }
        self.control.check()
    }

    fn accelerator_rows(
        &self,
        name: &str,
        field: &str,
        projection: Projection,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if projection == Projection::BlockMax {
            let _binding = reserve_bindings(self.control, &[name.as_bytes()])?;
            let versioned: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = 'scorer_fingerprint')",
                [name], |row| row.get(0),
            ).map_err(SQLiteError::from)?;
            if !versioned {
                return Ok(());
            }
        }
        let mut sql = BudgetedString::new(self.control.memory());
        sql.push_str(if projection == Projection::Skip {
            "SELECT term, skip_doc_id, skip_offset FROM \""
        } else {
            "SELECT term, block_idx, max_score, scorer_fingerprint FROM \""
        })?;
        for (offset, character) in name.chars().enumerate() {
            if offset % 1024 == 0 {
                self.control.check()?;
            }
            if character == '"' {
                sql.push('"')?;
            }
            sql.push(character)?;
        }
        sql.push('"')?;
        let _sql = self.control.memory().reserve(sql.len())?;
        let mut statement = self.connection.prepare(&sql).map_err(SQLiteError::from)?;
        let mut rows = statement.query([]).map_err(SQLiteError::from)?;
        while let Some(row) = rows.next().map_err(SQLiteError::from)? {
            self.control.check()?;
            let mut values = [ValueRef::Null; 6];
            values[0] = ValueRef::Text(self.table.as_bytes());
            values[1] = ValueRef::Text(field.as_bytes());
            let size = if projection == Projection::Skip { 5 } else { 6 };
            for (position, value) in values[2..size].iter_mut().enumerate() {
                *value = row.get_ref(position).map_err(SQLiteError::from)?;
            }
            self.project(&values[..size], projection, prefix, visit)?;
        }
        self.control.check()
    }
}
