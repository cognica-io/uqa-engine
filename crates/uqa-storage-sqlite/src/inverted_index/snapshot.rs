//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unbound physical captures project one `SQLite` read boundary into common immutable occurrence readers.

use super::{records, Arc, InvertedIndex, SQLiteError, SQLiteInvertedIndex, StorageBackendResult};
use crate::read_control::{read_snapshot, reserve_bindings};
use rusqlite::{types::ValueRef, Connection};
use uqa_core::memory::BudgetedString;
use uqa_storage::{
    key_value::{
        occurrence_format::{OccurrenceAddress as Address, OccurrenceProjection as Projection},
        KeyValueInvertedIndex, KeyValueRead, KeyValueReadRevision,
    },
    read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor},
};

mod accelerators;
#[cfg(test)]
mod tests;

struct PhysicalRead<'a> {
    connection: &'a Connection,
    table: &'a str,
    control: &'a StorageReadControl,
    revision: KeyValueReadRevision,
}

impl SQLiteInvertedIndex {
    pub(super) fn physical_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        control.check()?;
        Ok(self.conn.with(|connection| {
            read_snapshot(connection, |connection| {
                let read = PhysicalRead {
                    connection,
                    table: &self.table,
                    control,
                    revision: KeyValueReadRevision::fresh(),
                };
                KeyValueInvertedIndex::snapshot_from_read(&read, &self.table, &self.bindings)
                    .map_err(SQLiteError::from)
            })
        })?)
    }
}

impl PhysicalRead<'_> {
    fn rows(
        &self,
        projection: Projection,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if matches!(projection, Projection::Skip | Projection::BlockMax) {
            return self.accelerators(projection, prefix, visit);
        }
        let layout = records::family(projection)
            .ok_or_else(|| records::invalid("unknown occurrence row projection"))?
            .layout();
        let mut sql = BudgetedString::new(self.control.memory());
        sql.push_str("SELECT ")?;
        for (position, column) in layout.columns.iter().enumerate() {
            if position != 0 {
                sql.push(',')?;
            }
            sql.push_str(column)?;
        }
        sql.push_str(" FROM ")?;
        sql.push_str(layout.table)?;
        sql.push_str(" WHERE table_name = ?1")?;
        let _sql = self.control.memory().reserve(sql.len())?;
        let _binding = reserve_bindings(self.control, &[self.table.as_bytes()])?;
        let mut statement = self.connection.prepare(&sql).map_err(SQLiteError::from)?;
        let mut rows = statement.query([self.table]).map_err(SQLiteError::from)?;
        while let Some(row) = rows.next().map_err(SQLiteError::from)? {
            self.control.check()?;
            let mut values = [ValueRef::Null; 7];
            let values = values.get_mut(..layout.columns.len()).ok_or_else(|| {
                records::invalid("occurrence row exceeds its fixed column capacity")
            })?;
            for (position, value) in values.iter_mut().enumerate() {
                *value = row.get_ref(position).map_err(SQLiteError::from)?;
            }
            self.project(values, projection, prefix, visit)?;
        }
        self.control.check()
    }

    fn project(
        &self,
        values: &[ValueRef<'_>],
        projection: Projection,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let key = records::address_from_row(values, self.table, projection, self.control)?;
        if key.starts_with(prefix) {
            records::project(values, projection, self.control, |value| visit(&key, value))?;
        }
        Ok(())
    }

    fn legacy_presence(
        &self,
        projection: Projection,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let table = match projection {
            Projection::LegacyPosting | Projection::LegacyReverse => "_postings",
            Projection::LegacyScore | Projection::LegacyPositions => "_posting_clusters",
            Projection::LegacyDocument => "_posting_documents",
            Projection::LegacyLength => "_doc_lengths",
            Projection::LegacyField => "_field_stats",
            _ => {
                return Err(records::invalid(
                    "current projection used as a legacy marker",
                ))
            }
        };
        let _bindings = reserve_bindings(self.control, &[table.as_bytes(), self.table.as_bytes()])?;
        if super::table_exists(self.connection, table).map_err(SQLiteError::from)? {
            let mut sql = BudgetedString::new(self.control.memory());
            sql.push_str("SELECT EXISTS(SELECT 1 FROM ")?;
            sql.push_str(table)?;
            sql.push_str(" WHERE table_name = ?1)")?;
            let _sql = self.control.memory().reserve(sql.len())?;
            let found: bool = self
                .connection
                .query_row(&sql, [self.table], |row| row.get(0))
                .map_err(SQLiteError::from)?;
            if found {
                // The common legacy namespaces carry presence only; their values require source reconstruction.
                visit(prefix, &[])?;
            }
        }
        self.control.check()
    }
}

impl KeyValueRead for PhysicalRead<'_> {
    fn control(&self) -> &StorageReadControl {
        self.control
    }

    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.control.check()?;
        Ok(self.revision.clone())
    }

    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let mut found = false;
        self.visit_prefix(key, &mut |candidate, value| {
            if candidate == key {
                found = true;
                visit(Some(value))?;
            }
            Ok(())
        })?;
        if !found {
            visit(None)?;
        }
        Ok(())
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        let address = Address::decode(prefix)?;
        if address.table != self.table {
            return Err(records::invalid(
                "physical occurrence prefix belongs to another table",
            ));
        }
        let projections = address
            .projection
            .as_ref()
            .map_or(records::CURRENT.as_slice(), std::slice::from_ref);
        for &projection in projections {
            self.control.check()?;
            if projection.is_legacy() {
                self.legacy_presence(projection, prefix, visit)?;
            } else {
                self.rows(projection, prefix, visit)?;
            }
        }
        self.control.check()
    }
}
