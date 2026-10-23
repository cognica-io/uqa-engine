//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Columns collected across the pages of one latest-commit scan, charged to the cache's allowance from the first row.

use uqa_core::memory::{BudgetedVec, MemoryReservation};
use uqa_core::Value;
use uqa_storage::document_store::decoding::ProjectedDocumentFields;

use super::{grow, scalar_bytes, DecodedColumns};
use crate::mvcc::native::NativeRecordOwner;

pub(crate) struct ColumnBuilder {
    pub(super) table: String,
    pub(super) owner: NativeRecordOwner,
    pub(super) generation: i64,
    pub(super) fields: Vec<String>,
    source_positions: BudgetedVec<usize>,
    /// The last document collected, where the scan's next page must continue.
    last: Option<i64>,
    pub(super) ids: BudgetedVec<i64>,
    pub(super) columns: Vec<BudgetedVec<Value>>,
    pub(super) strings: MemoryReservation,
}

impl ColumnBuilder {
    /// Start collecting the distinct `fields` of `table` at `generation` from its first row. Entries of the table that no later read can observe are dropped first.
    pub(crate) fn start(
        cache: &DecodedColumns,
        table: &str,
        owner: NativeRecordOwner,
        generation: i64,
        fields: &[&str],
    ) -> Option<Self> {
        cache.retire_stale(table, owner, generation);
        let mut distinct: Vec<String> = Vec::with_capacity(fields.len());
        let mut source_positions = BudgetedVec::new(cache.budget());
        for (position, field) in fields.iter().enumerate() {
            if !distinct.iter().any(|name| name == field) {
                source_positions.push(position).ok()?;
                distinct.push((*field).to_owned());
            }
        }
        let columns = distinct
            .iter()
            .map(|_| BudgetedVec::new(cache.budget()))
            .collect();
        Some(Self {
            table: table.to_owned(),
            owner,
            generation,
            fields: distinct,
            source_positions,
            last: None,
            ids: BudgetedVec::new(cache.budget()),
            columns,
            strings: cache.budget().empty_reservation(),
        })
    }

    /// A resumed page may request the same fields in a different order.
    pub(super) fn bind_projection(&mut self, fields: &[&str]) {
        for (name, position) in self.fields.iter().zip(self.source_positions.iter_mut()) {
            *position = fields
                .iter()
                .position(|field| *field == name)
                .expect("resumed projection contains every cached field");
        }
    }

    /// Whether a page of `table` at `generation` selecting `fields` after `after` continues this build.
    pub(super) fn continues(
        &self,
        table: &str,
        owner: NativeRecordOwner,
        generation: i64,
        fields: &[&str],
        after: Option<i64>,
    ) -> bool {
        self.table == table
            && self.owner == owner
            && self.generation == generation
            && after.is_some()
            && self.last == after
            && fields
                .iter()
                .all(|field| self.fields.iter().any(|name| name == field))
            && self
                .fields
                .iter()
                .all(|name| fields.contains(&name.as_str()))
    }

    /// Move the selected values out of one decoded row after its visitor returned. Returns false, abandoning the build, when a value is not a cacheable scalar or the allowance cannot hold the row even after evicting other tables.
    pub(crate) fn push(
        &mut self,
        cache: &DecodedColumns,
        id: i64,
        row: &mut ProjectedDocumentFields,
    ) -> bool {
        if !self.reserve_row(cache) {
            return false;
        }
        for (position, column) in self.source_positions.iter().zip(&mut self.columns) {
            let value = row
                .get_mut(*position)
                .map_or(Value::Null, |value| std::mem::replace(value, Value::Null));
            let Some(bytes) = scalar_bytes(&value) else {
                return false;
            };
            if grow(cache, &self.table, &mut self.strings, bytes).is_err() {
                return false;
            }
            if column.push(value).is_err() {
                return false;
            }
        }
        if self.ids.push(id).is_err() {
            return false;
        }
        self.last = Some(id);
        true
    }

    fn reserve_row(&mut self, cache: &DecodedColumns) -> bool {
        loop {
            let reserved = self.ids.reserve(1).and_then(|()| {
                self.columns
                    .iter_mut()
                    .try_for_each(|column| column.reserve(1))
            });
            match reserved {
                Ok(()) => return true,
                Err(_) if cache.evict_other(&self.table) => {}
                Err(_) => return false,
            }
        }
    }
}
