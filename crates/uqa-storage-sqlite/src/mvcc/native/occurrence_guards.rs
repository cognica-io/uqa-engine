//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent object-scoped occurrence document and structural preconditions.

use super::{
    NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot,
};
use crate::connection::Result;
use rusqlite::types::ValueRef;
use uqa_storage::KeyValueBatch;

pub(super) const SQL: &str = "CREATE TABLE _uqa_mvcc_native_occurrence_guards (table_name TEXT NOT NULL, document_id INTEGER NOT NULL CHECK(document_id >= -1), PRIMARY KEY(table_name, document_id)) WITHOUT ROWID";

impl NativeSnapshot {
    pub(crate) fn occurrence_guard(
        &self,
        batch: &mut dyn KeyValueBatch,
        table: &str,
        owner: NativeRecordOwner,
        document: i64,
    ) -> Result<()> {
        self.put_row(
            batch,
            Family::OccurrenceGuards,
            owner,
            &[
                ValueRef::Text(table.as_bytes()),
                ValueRef::Integer(document),
            ],
        )
    }

    pub(crate) fn reset_occurrence_rows(
        &self,
        batch: &mut dyn KeyValueBatch,
        owner: NativeRecordOwner,
    ) -> Result<()> {
        // An absent structural row still retains a versioned tombstone. Its identity survives empty clears and never needs a table-name payload during rename or owner retirement.
        let key = NativeRecordIdentity::new(Family::OccurrenceGuards, owner)?
            .encode_key(&[ValueRef::Integer(-1)], &self.control)?;
        batch.fence_record(&key)?;
        let key = NativeRecordIdentity::new(Family::OccurrenceFormats, owner)?
            .encode_key(&[], &self.control)?;
        batch.fence_record(&key)?;
        Ok(())
    }
}
