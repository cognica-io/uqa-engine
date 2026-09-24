//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identity-only reads keep native and legacy output buffers under the invoking allowance.

use rusqlite::params;
use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::read_control::StorageReadControl;

use crate::document_store::{
    document_id_from_sqlite, sqlite_doc_id, SQLiteDocumentStore, SQLiteResult,
};

impl SQLiteDocumentStore {
    pub(in crate::document_store) fn read_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> SQLiteResult<BudgetedVec<DocId>> {
        control.check()?;
        if limit == 0 {
            return Ok(BudgetedVec::new(control.memory()));
        }
        if let Some(ids) = self.read_native_with_control(Some(control), |read| {
            read.id_page_controlled(after, limit, control)
        })? {
            return Ok(ids);
        }
        let after = after.map(sqlite_doc_id).transpose()?;
        // A negative SQLite limit is unbounded; the caller's allowance still bounds every emitted identity.
        let limit = i64::try_from(limit).unwrap_or(-1);
        self.conn.with(|connection| {
            control.check()?;
            let sql = if after.is_some() {
                "SELECT doc_id FROM _documents WHERE table_name = ?1 AND doc_id > ?2 ORDER BY doc_id LIMIT ?3"
            } else {
                "SELECT doc_id FROM _documents WHERE table_name = ?1 ORDER BY doc_id LIMIT ?3"
            };
            let mut statement = connection.prepare_cached(sql)?;
            let mut rows = statement.query(params![self.table, after, limit])?;
            let mut ids = BudgetedVec::new(control.memory());
            while let Some(row) = rows.next()? {
                control.check()?;
                ids.push(document_id_from_sqlite(row.get(0)?)?)?;
            }
            control.check()?;
            Ok(ids)
        })
    }
}
