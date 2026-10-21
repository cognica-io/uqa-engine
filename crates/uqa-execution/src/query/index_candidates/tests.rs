//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mutation::overlay::CommandIndexProbe;
use crate::query::document_changes::DocumentChanges;
use uqa_core::{DocId, Payload, PostingEntry, PostingList, Predicate, Value};

mod commands;

struct Index;
impl IndexCandidates<'_> {
    fn test_ids(&self, filter: &ScalarExpr) -> Result<Option<Vec<DocId>>, SQLError> {
        self.select(filter)?
            .map(|ids| ids.cursor().collect())
            .transpose()
    }
}

impl QueryIndexRead for Index {
    fn visit_value_index(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        let Some(posting) = self.value_index_scan(table, field, predicate)? else {
            return Ok(false);
        };
        for entry in posting.entries() {
            visit(entry.doc_id)?;
        }
        Ok(true)
    }

    fn visit_exact_command_matches(
        &self,
        _: &str,
        _: &str,
        _: &Value,
        _: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<DocumentChanges, SQLError> {
        unreachable!("unchanged view")
    }

    fn value_index_scan(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        assert_eq!(table, "t");
        if field != "v" {
            return Ok(None);
        }
        assert_eq!(predicate, &Predicate::Equals(Value::Int(7)));
        Ok(Some(PostingList::from_sorted_unchecked(vec![
            PostingEntry::new(42, Payload::default()),
        ])))
    }
    fn command_overlay_changes(&self, _: &str) -> Result<Option<DocumentChanges>, SQLError> {
        Ok(None)
    }
    fn exact_command_matches(
        &self,
        _: &str,
        _: &str,
        _: &Value,
    ) -> Result<CommandIndexProbe, SQLError> {
        unreachable!("unchanged view")
    }
}

fn equal(column: &str, value: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: "alias".into(),
            column: column.into(),
        }),
        rhs: Box::new(value),
    }
}

#[test]
fn mapped_columns_bind_parameters_and_skip_unanswerable_conjuncts() {
    let uqa_sql::Statement::CreateTable(table) =
        uqa_sql::compile("CREATE TABLE t(v integer, unindexed integer)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    let cancellation = CancellationToken::new();
    let select = IndexCandidates {
        reads: &Index,
        table: "t",
        columns: &table.columns,
        visible: &|name| {
            if name == "v" {
                "renamed".into()
            } else {
                name.into()
            }
        },
        params: &[SQLParam::scalar(Value::Int(7))],
        command_visible: true,
        cancellation: &cancellation,
        bytes: 64 * 1024,
    };
    let predicate = equal("renamed", ScalarExpr::Param(1));
    assert_eq!(select.test_ids(&predicate).unwrap(), Some(vec![42]));
    assert_eq!(
        select
            .test_ids(&ScalarExpr::And(vec![
                equal("unindexed", ScalarExpr::Literal(Value::Int(0))),
                predicate.clone()
            ]))
            .unwrap(),
        Some(vec![42])
    );
    assert_eq!(
        select.test_ids(&equal("v", ScalarExpr::Param(1))).unwrap(),
        None
    );
    assert_eq!(
        select
            .test_ids(&equal("_doc_id", ScalarExpr::Param(1)))
            .unwrap(),
        None
    );
    assert_eq!(
        select
            .test_ids(&equal("renamed", ScalarExpr::Column("unindexed".into())))
            .unwrap(),
        None
    );
    assert_eq!(
        select
            .test_ids(&ScalarExpr::Or(vec![
                predicate,
                equal("unindexed", ScalarExpr::Param(1))
            ]))
            .unwrap(),
        None
    );
    cancellation.cancel();
    assert!(select
        .test_ids(&equal("renamed", ScalarExpr::Param(1)))
        .is_err());
}

#[test]
fn selected_operand_casts_preserve_index_candidates_and_residual_equality() {
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile("CREATE TABLE t(v bigint)")
        .unwrap()
        .remove(0)
    else {
        unreachable!()
    };
    let schema = crate::RowSchema::with_qualified_types(
        "alias",
        vec!["renamed".into()],
        vec![Some(uqa_sql::ColumnType::BigInteger)],
    );
    let cancellation = CancellationToken::new();
    for (value, expected) in [
        (Value::Int(7), Some(vec![42])),
        (Value::Float(9_007_199_254_740_992.0), None),
    ] {
        let params = [SQLParam::scalar(value)];
        let filter = uqa_sql::bind_type_introspection(
            equal("renamed", ScalarExpr::Param(1)),
            &schema,
            &params,
        );
        let selected = IndexCandidates {
            reads: &Index,
            table: "t",
            columns: &table.columns,
            visible: &|_| "renamed".into(),
            params: &params,
            command_visible: false,
            cancellation: &cancellation,
            bytes: 64 * 1024,
        }
        .test_ids(&filter)
        .unwrap();
        assert_eq!(selected, expected);
        let predicate = crate::ProjectedPredicate::compile_bound(&filter, &schema, &params)
            .unwrap()
            .unwrap();
        assert_eq!(
            predicate
                .keep(&[&Value::Int(9_007_199_254_740_993)])
                .unwrap(),
            expected.is_none()
        );
    }
}

struct ManyMatches;

impl QueryIndexRead for ManyMatches {
    fn visit_value_index(
        &self,
        _: &str,
        _: &str,
        _: &Predicate,
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        for id in (0..65_536).rev() {
            visit(id)?;
        }
        Ok(true)
    }

    fn visit_exact_command_matches(
        &self,
        _: &str,
        _: &str,
        _: &Value,
        _: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<DocumentChanges, SQLError> {
        unreachable!("unchanged view")
    }

    fn value_index_scan(
        &self,
        _: &str,
        _: &str,
        _: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        panic!("candidate capture must not materialize postings")
    }

    fn command_overlay_changes(&self, _: &str) -> Result<Option<DocumentChanges>, SQLError> {
        Ok(None)
    }

    fn exact_command_matches(
        &self,
        _: &str,
        _: &str,
        _: &Value,
    ) -> Result<CommandIndexProbe, SQLError> {
        unreachable!("unchanged view")
    }
}

#[test]
fn selected_identity_capture_preserves_full_width_order_and_independent_cursors() {
    for bytes in [0, 64 * 1024] {
        let token = CancellationToken::new();
        let ids = SelectedIds::capture(
            [u64::MAX, 0, 1 << 63, 7, 0, u64::MAX].into_iter().map(Ok),
            bytes,
            &token,
        )
        .unwrap();
        assert_eq!(ids.len(), 4);
        let mut first = ids.cursor();
        assert_eq!(first.next().unwrap().unwrap(), 0);
        let second = ids.cursor();
        drop(ids);
        assert_eq!(
            second.collect::<Result<Vec<_>, _>>().unwrap(),
            [0, 7, 1 << 63, u64::MAX]
        );
        assert_eq!(
            first.collect::<Result<Vec<_>, _>>().unwrap(),
            [7, 1 << 63, u64::MAX]
        );
    }
}

#[test]
fn selected_identity_capture_propagates_late_errors_and_cursor_cancellation() {
    for bytes in [0, 64 * 1024] {
        let token = CancellationToken::new();
        let error = SelectedIds::capture(
            [
                Ok(9),
                Err(SQLError::Routine {
                    sqlstate: "XX001".into(),
                    message: "late index error".into(),
                }),
            ],
            bytes,
            &token,
        )
        .err()
        .expect("source error");
        assert_eq!(error.sqlstate(), Some("XX001"));
        let ids = SelectedIds::capture([Ok(1), Ok(2)], bytes, &token).unwrap();
        let mut cursor = ids.cursor();
        assert_eq!(cursor.next().unwrap().unwrap(), 1);
        token.cancel();
        assert_eq!(
            cursor.next().unwrap().unwrap_err().sqlstate(),
            Some("57014")
        );
        assert!(cursor.next().is_none());
        assert!(SelectedIds::capture(std::iter::empty(), bytes, &token).is_err());
    }
}

#[test]
fn small_selected_identity_capture_does_not_allocate_spill_or_sort_buffers() {
    let cancellation = CancellationToken::new();
    let allocation = allocation_counter::measure(|| {
        let selected = SelectedIds::capture([Ok(42)], 64 * 1024 * 1024, &cancellation).unwrap();
        assert_eq!(selected.cursor().next().unwrap().unwrap(), 42);
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
}

#[test]
fn indexed_scan_candidates_capture_large_support_without_posting_materialization() {
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile("CREATE TABLE t(v integer)")
        .unwrap()
        .remove(0)
    else {
        unreachable!()
    };
    let cancellation = CancellationToken::new();
    let select = IndexCandidates {
        reads: &ManyMatches,
        table: "t",
        columns: &table.columns,
        visible: &str::to_owned,
        params: &[],
        command_visible: true,
        cancellation: &cancellation,
        bytes: 64 * 1024,
    };
    let predicate = equal("v", ScalarExpr::Literal(Value::Int(7)));
    let allocation = allocation_counter::measure(|| {
        let candidates = select.select(&predicate).unwrap().unwrap();
        assert_eq!(candidates.len(), 65_536);
        for (expected, id) in candidates.cursor().enumerate() {
            assert_eq!(id.unwrap(), expected as u64);
        }
    });
    assert!(allocation.bytes_max < 1024 * 1024, "{allocation:?}");
}
