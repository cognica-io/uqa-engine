//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn expression(sql: &str) -> Expr {
    let crate::Statement::CreateTable(mut table) =
        crate::compile(&format!("CREATE TABLE t(a bigint DEFAULT ({sql}))"))
            .unwrap()
            .remove(0)
    else {
        panic!("table")
    };
    table.columns.remove(0).default.unwrap()
}

#[test]
fn sequence_observation_borrows_large_unrelated_payloads_without_allocating() {
    let expression = Expr::Array(vec![
        Expr::Literal(Value::Str("x".repeat(4 << 20))),
        expression("nextval('first')"),
        expression("currval('second')"),
    ]);
    let budget = MemoryBudget::new(0);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&budget, &statement, &session);
    let mut names = Vec::with_capacity(2);
    let measured = allocation_counter::measure(|| {
        visit_sequence_function_references_with_control(&expression, &control, &mut |reference| {
            names.push(if reference == "first" {
                1
            } else if reference == "second" {
                2
            } else {
                panic!("reference")
            });
            Ok(())
        })
        .unwrap();
    });
    assert_eq!(names, [1, 2]);
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(budget.used(), 0);
    for token in [&statement, &session] {
        token.cancel();
        assert_eq!(
            visit_sequence_function_references_with_control(
                &expression,
                &control,
                &mut |_| panic!("cancelled traversal")
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        token.reset();
    }
}

#[test]
fn sequence_observation_preserves_child_order_and_stops_at_the_first_callback_error() {
    let source = expression("sum(nextval('argument')) FILTER(WHERE currval('filter')>0) OVER(PARTITION BY nextval('partition') ORDER BY currval('ordering') ROWS BETWEEN nextval('start') PRECEDING AND nextval('end') FOLLOWING)");
    let expected = [
        "argument",
        "filter",
        "partition",
        "ordering",
        "start",
        "end",
    ];
    let mut ordinary = source.clone();
    let mut observed = Vec::new();
    super::super::rewrite_sequence_function_references(&mut ordinary, &mut |reference| {
        observed.push(reference.clone());
        Ok(())
    })
    .unwrap();
    assert_eq!(observed, expected);
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut count = 0;
    visit_sequence_function_references_with_control(&source, &control, &mut |reference| {
        assert_eq!(reference, expected[count]);
        count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, expected.len());
    for stop in 0..expected.len() {
        let mut count = 0;
        let error =
            visit_sequence_function_references_with_control(&source, &control, &mut |reference| {
                assert_eq!(reference, expected[count]);
                count += 1;
                if count == stop + 1 {
                    Err(SQLError::Internal("callback failed".into()))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(error.to_string().contains("callback failed"));
        assert_eq!(count, stop + 1);
        assert_eq!(budget.used(), 0);
    }
    let subquery = expression("nextval('before')+(SELECT 1)+nextval('after')");
    let mut visited = 0;
    let error =
        visit_sequence_function_references_with_control(&subquery, &control, &mut |reference| {
            assert_eq!(reference, "before");
            visited += 1;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(visited, 1);
    assert!(error
        .to_string()
        .contains("schema expression contains a subquery"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn stored_sequence_reference_errors_admit_their_selected_text_before_copying() {
    use crate::schema::sequences::names::{
        resolve_stored_sequence_reference, resolve_stored_sequence_reference_with_control,
        StoredSequenceRegistry,
    };
    struct Empty;
    impl StoredSequenceRegistry for Empty {
        fn contains_sequence(&self, _: &uqa_core::RelationIdentity) -> bool {
            false
        }
        fn sequence_names(&self) -> Vec<uqa_core::RelationIdentity> {
            Vec::new()
        }
    }
    let budget = MemoryBudget::new(64 << 10);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&budget, &statement, &session);
    let sibling = control.copy_text("sibling").unwrap();
    let baseline = budget.used();
    for reference in ["x".repeat(4 << 20), format!("\"{}", "x".repeat(4 << 20))] {
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                resolve_stored_sequence_reference_with_control(&Empty, &reference, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        // Diagnostic formatting streams admitted chunks until the allowance is exhausted.
        assert!(measured.bytes_max <= 64 << 10, "{measured:?}");
        assert!(measured.bytes_total < 128 << 10, "{measured:?}");
        assert_eq!(budget.used(), baseline);
    }
    for reference in ["missing", "\"invalid"] {
        let ordinary = resolve_stored_sequence_reference(&Empty, reference).unwrap_err();
        let controlled =
            resolve_stored_sequence_reference_with_control(&Empty, reference, &control)
                .unwrap_err();
        assert_eq!(
            controlled.to_string(),
            SQLError::Internal(ordinary).to_string()
        );
        assert_eq!(budget.used(), baseline);
    }
    for token in [&statement, &session] {
        token.cancel();
        assert_eq!(
            resolve_stored_sequence_reference_with_control(&Empty, "missing", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), baseline);
        token.reset();
    }
    assert_eq!(&**sibling, "sibling");
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
