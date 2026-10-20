//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::{document_store::Document, read_control::StorageReadControl};

struct Memory(usize);
impl QueryMemorySettings for Memory {
    fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        Ok(self.0)
    }
}

struct Rows {
    remaining: Cell<u64>,
    captured: Cell<bool>,
    reads: Cell<u64>,
    pages: Cell<usize>,
    missing: Option<DocId>,
    invalid: Option<DocId>,
    fail_page: bool,
}

impl Rows {
    fn new(count: u64) -> Self {
        Self {
            remaining: Cell::new(count),
            captured: Cell::new(false),
            reads: Cell::new(0),
            pages: Cell::new(0),
            missing: None,
            invalid: None,
            fail_page: false,
        }
    }
}

impl MutationRead for Rows {
    fn table_doc_ids(&self, _: &str) -> Result<Vec<DocId>, SQLError> {
        panic!("whole-table selected identity materialization");
    }
    fn live_table_doc_ids(&self, _: &str) -> Result<Vec<DocId>, SQLError> {
        panic!("whole-table live identity materialization");
    }
    fn table_doc_id_page(
        &self,
        _: &str,
        _: Option<DocId>,
        _: usize,
        _: &StorageReadControl,
    ) -> Result<BudgetedVec<DocId>, SQLError> {
        panic!("validation must retain its live identity view");
    }
    fn live_table_doc_id_page(
        &self,
        _: &str,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> Result<BudgetedVec<DocId>, SQLError> {
        assert_eq!(self.reads.get(), 0, "all identities precede row evaluation");
        assert!(limit <= 128);
        self.pages.set(self.pages.get() + 1);
        if self.fail_page && after.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "XX001".into(),
                message: "identity page failed".into(),
            });
        }
        let mut page = BudgetedVec::new(control.memory());
        for id in (after.unwrap_or(0)..self.remaining.get())
            .take(limit)
            .map(|id| id + 1)
        {
            page.push(id).unwrap();
        }
        if page.is_empty() {
            self.captured.set(true);
        }
        Ok(page)
    }
    fn get_document(&self, _: &str, id: DocId) -> Result<Option<Document>, SQLError> {
        assert!(self.captured.get(), "row effects cannot precede capture");
        self.reads.set(self.reads.get() + 1);
        assert_eq!(id, self.reads.get(), "identity order must be preserved");
        self.remaining.set(0);
        if self.missing == Some(id) {
            return Ok(None);
        }
        Ok(Some(Document::from([
            ("first".into(), Value::Int(1)),
            (
                "second".into(),
                if self.invalid == Some(id) {
                    Value::Null
                } else {
                    Value::Int(2)
                },
            ),
        ])))
    }
    fn raw_document(&self, _: &str, _: DocId) -> Result<Option<Document>, SQLError> {
        panic!("validation retains its existing document semantics");
    }
    fn command_overlay_changes(
        &self,
        _: &str,
    ) -> Result<Option<crate::query::document_changes::DocumentChanges>, SQLError> {
        panic!("the selected live page already owns overlay visibility");
    }
}

#[test]
fn constraint_validation_spills_initial_identities_before_row_evaluation() {
    for (count, allowance) in [(65_536, 64 * 1024), (32, 0), (0, 0)] {
        let rows = Rows::new(count);
        let allocation = allocation_counter::measure(|| {
            validate_not_null_columns(
                &rows,
                &Memory(allowance),
                &CancellationToken::new(),
                "items",
                &["first", "second"],
            )
            .unwrap();
        });
        assert_eq!(rows.reads.get(), count);
        assert!(rows.pages.get() <= usize::try_from(count / 128 + 2).unwrap());
        assert!(allocation.bytes_max < 512 * 1024, "{allocation:?}");
    }
}

#[test]
fn constraint_validation_preserves_missing_rows_and_first_invalid_column() {
    let mut rows = Rows::new(256);
    rows.missing = Some(1);
    rows.invalid = Some(3);
    let error = validate_not_null_columns(
        &rows,
        &Memory(64),
        &CancellationToken::new(),
        "items",
        &["first", "second"],
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23502"));
    assert!(error
        .to_string()
        .contains("column \"second\" of relation \"items\" contains null values"));
    assert_eq!(rows.reads.get(), 3);
}

#[test]
fn constraint_identity_capture_propagates_cancellation_and_page_failure_before_rows() {
    let mut rows = Rows::new(256);
    rows.fail_page = true;
    let cancellation = CancellationToken::new();
    let error =
        validate_not_null_rows(&rows, &Memory(0), &cancellation, "items", "first").unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX001"));
    assert_eq!(rows.reads.get(), 0);
    let pages = rows.pages.get();
    cancellation.cancel();
    let error =
        validate_not_null_rows(&rows, &Memory(0), &cancellation, "items", "first").unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(rows.reads.get(), 0);
    assert_eq!(rows.pages.get(), pages);
}
