//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{collections::BTreeMap, sync::Mutex};
use uqa_core::{CancellationToken, Value};
use uqa_sql::ast::{ColumnDef, ColumnType};
use uqa_storage::StorageBackendResult;

struct Rows {
    values: BTreeMap<DocId, Value>,
    reads: Mutex<Vec<Vec<DocId>>>,
    fail: bool,
    fail_at: Option<DocId>,
}

impl RetrievalRelations for Rows {
    fn serializable_read(
        &self,
        _: &str,
    ) -> Result<Option<crate::serializable::SerializableRelationRead>, SQLError> {
        unreachable!("the driver retains the logical predicate observation")
    }

    fn try_describe_query_table(&self, _: &str) -> StorageBackendResult<Option<Vec<ColumnDef>>> {
        unreachable!("the driver validates the field before evaluating candidates")
    }

    fn has_table(&self, _: &str) -> StorageBackendResult<bool> {
        unreachable!("candidate filtering does not refresh catalog state")
    }

    fn column_type(&self, _: &str, _: &str) -> StorageBackendResult<Option<ColumnType>> {
        unreachable!("candidate filtering does not refresh types")
    }

    fn table_doc_ids(&self, _: &str) -> Result<Vec<DocId>, SQLError> {
        panic!("candidate filtering must not enumerate the whole relation")
    }
    fn table_doc_id_page(
        &self,
        _: &str,
        _: Option<DocId>,
        _: usize,
        _: &uqa_storage::read_control::StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError> {
        panic!("candidate filtering must not enumerate the whole relation")
    }

    fn get_document_fields(
        &self,
        table: &str,
        ids: &[DocId],
        field: &str,
    ) -> Result<BTreeMap<DocId, Value>, SQLError> {
        assert_eq!((table, field), ("records", "value"));
        self.reads.lock().unwrap().push(ids.to_vec());
        if self.fail || self.fail_at.is_some_and(|id| ids.contains(&id)) {
            return Err(SQLError::Internal("document read failed".into()));
        }
        Ok(ids
            .iter()
            .filter_map(|id| self.values.get(id).map(|value| (*id, value.clone())))
            .collect())
    }
}

fn rows() -> Rows {
    Rows {
        values: [(2, Value::Null), (9, Value::Int(7)), (20, Value::Int(7))].into(),
        reads: Mutex::new(Vec::new()),
        fail: false,
        fail_at: None,
    }
}

#[test]
fn captured_filter_reads_only_selected_fields_and_preserves_null_membership() {
    for (predicate, expected) in [
        (Predicate::IsNull, vec![2]),
        (Predicate::IsNotNull, vec![9]),
        (Predicate::Equals(Value::Int(7)), vec![9]),
    ] {
        let rows = rows();
        let cancellation = CancellationToken::new();
        let ids = crate::query::SelectedIds::capture([Ok(2), Ok(9)], 0, &cancellation).unwrap();
        let result = capture(
            &rows,
            "records",
            "value",
            &predicate,
            &ids,
            0,
            &cancellation,
        )
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            assert_eq!(entry.score.to_bits(), 0.0_f64.to_bits());
            entry.doc_id
        })
        .collect::<Vec<_>>();
        assert_eq!(result, expected);
        assert_eq!(*rows.reads.lock().unwrap(), vec![vec![2], vec![9]]);
    }
}

#[test]
fn captured_filter_preserves_read_errors_before_missing_document_diagnostics() {
    let cancellation = CancellationToken::new();
    let ids = crate::query::SelectedIds::capture([Ok(5), Ok(9)], 0, &cancellation).unwrap();
    for fail_at in [None, Some(9)] {
        let mut rows = rows();
        rows.fail_at = fail_at;
        let error = capture(
            &rows,
            "records",
            "value",
            &Predicate::IsNull,
            &ids,
            0,
            &cancellation,
        )
        .err()
        .unwrap();
        assert_eq!(*rows.reads.lock().unwrap(), vec![vec![5], vec![9]]);
        if fail_at.is_some() {
            assert!(error.to_string().contains("document read failed"));
        } else {
            assert!(error.to_string().contains("candidate 5 is missing"));
        }
    }
    let rows = rows();
    cancellation.cancel();
    let error = capture(
        &rows,
        "records",
        "value",
        &Predicate::IsNull,
        &ids,
        0,
        &cancellation,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert!(rows.reads.lock().unwrap().is_empty());
}

#[test]
fn captured_filter_does_not_retain_wide_projected_values() {
    let mut rows = rows();
    rows.values = (0..513)
        .map(|id| (id, Value::Str("x".repeat(32 * 1024))))
        .collect();
    let cancellation = CancellationToken::new();
    let ids = crate::query::SelectedIds::capture((0..513).map(Ok), 4096, &cancellation).unwrap();
    let allocated = allocation_counter::measure(|| {
        let output = capture(
            &rows,
            "records",
            "value",
            &Predicate::IsNotNull,
            &ids,
            4096,
            &cancellation,
        )
        .unwrap();
        for (id, entry) in output.enumerate() {
            assert_eq!(entry.unwrap().doc_id, id as u64);
        }
    });
    // The source retains more than 16MiB; filtering may own only one projected value and bounded score storage.
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
}

#[test]
fn intersection_support_preserves_relational_absence_and_null_semantics() {
    let rows = rows();
    let candidates = [2, 5, 9];
    for (predicate, expected) in [
        (Predicate::IsNull, vec![2]),
        (Predicate::IsNotNull, vec![9]),
        (Predicate::Equals(Value::Int(7)), vec![9]),
        (Predicate::NotEquals(Value::Int(8)), vec![9]),
    ] {
        let output = evaluate(
            &rows,
            "records",
            "value",
            &predicate,
            Candidates::Intersection(&candidates),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            output.entries(),
            expected
                .into_iter()
                .map(|id| PostingEntry::new(id, Payload::default()))
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(*rows.reads.lock().unwrap(), vec![candidates.to_vec(); 4]);
}

#[test]
fn promised_document_rows_still_reject_an_inconsistent_snapshot() {
    let error = evaluate(
        &rows(),
        "records",
        "value",
        &Predicate::IsNull,
        Candidates::Documents(&[2, 5, 9]),
        || Ok(()),
    )
    .unwrap_err();
    assert!(
        matches!(error, SQLError::Internal(message) if message.contains("candidate 5 is missing"))
    );
}

#[test]
fn bounded_reads_and_cancellation_errors_are_not_empty_matches() {
    let mut rows = rows();
    rows.fail = true;
    let error = evaluate(
        &rows,
        "records",
        "value",
        &Predicate::IsNull,
        Candidates::Intersection(&[5]),
        || Ok(()),
    )
    .unwrap_err();
    assert!(matches!(error, SQLError::Internal(message) if message == "document read failed"));
    let token = CancellationToken::new();
    token.cancel();
    let error = evaluate(
        &rows,
        "records",
        "value",
        &Predicate::IsNull,
        Candidates::Intersection(&[5]),
        || token.check().map_err(Into::into),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(*rows.reads.lock().unwrap(), vec![vec![5]]);
}
