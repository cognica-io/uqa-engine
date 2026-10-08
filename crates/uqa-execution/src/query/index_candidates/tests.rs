//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mutation::overlay::CommandIndexProbe;
use crate::query::document_changes::DocumentChanges;
use uqa_core::{Payload, PostingEntry, PostingList, Predicate, Value};

struct Index;
impl QueryIndexRead for Index {
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
    };
    let predicate = equal("renamed", ScalarExpr::Param(1));
    assert_eq!(select.select(&predicate).unwrap(), Some(vec![42]));
    assert_eq!(
        select
            .select(&ScalarExpr::And(vec![
                equal("unindexed", ScalarExpr::Literal(Value::Int(0))),
                predicate.clone()
            ]))
            .unwrap(),
        Some(vec![42])
    );
    assert_eq!(
        select.select(&equal("v", ScalarExpr::Param(1))).unwrap(),
        None
    );
    assert_eq!(
        select
            .select(&equal("_doc_id", ScalarExpr::Param(1)))
            .unwrap(),
        None
    );
    assert_eq!(
        select
            .select(&equal("renamed", ScalarExpr::Column("unindexed".into())))
            .unwrap(),
        None
    );
    assert_eq!(
        select
            .select(&ScalarExpr::Or(vec![
                predicate,
                equal("unindexed", ScalarExpr::Param(1))
            ]))
            .unwrap(),
        None
    );
    cancellation.cancel();
    assert!(select
        .select(&equal("renamed", ScalarExpr::Param(1)))
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
        }
        .select(&filter)
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
