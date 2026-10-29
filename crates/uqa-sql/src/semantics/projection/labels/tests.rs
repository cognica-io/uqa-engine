//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn query(sql: &str) -> QueryPlan {
    let crate::plan::UnifiedPlan::Query(query) =
        crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    *query
}

#[test]
fn controlled_output_labels_keep_quoting_aliases_ordinals_and_duplicates() {
    for (sql, aliases, expected) in [
        (
            "SELECT a, q.b, 1, 2+3, lower('A'), pg_catalog.upper('b'), sum(a) OVER ()",
            vec![],
            vec!["a", "b", "?column?", "?column?", "lower", "upper", "sum"],
        ),
        (
            "SELECT \"Name With Spaces\"(1), custom.\"Mixed.Name\"(1), custom.\"a\"\"b\"(1)",
            vec![],
            vec!["Name With Spaces", "Mixed.Name", "a\"b"],
        ),
        (
            "SELECT a COLLATE \"C\", (q).f, *, q.*",
            vec![],
            vec!["a", "f", "*", "*"],
        ),
        (
            "SELECT a AS same, b AS same, c UNION ALL SELECT d, e, f",
            vec!["renamed".into()],
            vec!["renamed", "same", "c"],
        ),
        (
            "VALUES (1,2,3), (4,5,6)",
            vec!["first".into()],
            vec!["first", "column2", "column3"],
        ),
    ] {
        let query = query(sql);
        let budget = MemoryBudget::new(1024);
        let token = CancellationToken::new();
        let mut actual = Vec::new();
        assert!(visit_query_output_columns(
            &query,
            &aliases,
            &ProductionControl::new(&budget, &token, &token),
            &mut |name| {
                actual.push(name.to_owned());
                Ok(())
            },
        )
        .unwrap());
        assert_eq!(actual, expected, "{sql}");
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn simple_output_labels_borrow_input_storage_without_an_allowance() {
    let mut query = query("SELECT a, q.b, 1 AS named, (q).field");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    // Exercise the underlying column and field names independently of compiler-supplied aliases.
    block.projections[1].alias = None;
    block.projections[3].alias = None;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block");
    };
    let aliases = ["overridden".to_owned()];
    let ScalarExpr::QualifiedColumn { column, .. } = &block.projections[1].expr else {
        panic!("qualified column");
    };
    let ScalarExpr::Func { args, .. } = &block.projections[3].expr else {
        panic!("field selection");
    };
    let pointers = [
        aliases[0].as_ptr(),
        column.as_ptr(),
        block.projections[2].alias.as_ref().unwrap().as_ptr(),
        args[1].field_name().unwrap().as_ptr(),
    ];
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let mut index = 0;
    let allocation = allocation_counter::measure(|| {
        visit_query_output_columns(
            &query,
            &aliases,
            &ProductionControl::new(&budget, &token, &token),
            &mut |name| {
                assert_eq!(name.as_ptr(), pointers[index]);
                index += 1;
                Ok(())
            },
        )
        .unwrap();
    });
    assert_eq!(index, pointers.len());
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    assert_eq!(budget.peak(), 0);
}

#[test]
fn decoded_output_labels_are_admitted_and_released_between_callbacks() {
    let mut query = query("SELECT custom.\"a\"\"b\"(1)");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    block.projections.resize(1024, block.projections[0].clone());
    let parent = MemoryBudget::new(512);
    let sibling = parent.reserve(256).unwrap();
    let budget = parent.child(256);
    let token = CancellationToken::new();
    let mut calls = 0;
    visit_query_output_columns(
        &query,
        &[],
        &ProductionControl::new(&budget, &token, &token),
        &mut |name| {
            assert_eq!(name, "a\"b");
            assert!(budget.used() > 0);
            assert_eq!(parent.used(), sibling.bytes() + budget.used());
            calls += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(calls, 1024);
    assert_eq!(budget.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
    let denied = parent.child(0);
    let error = visit_query_output_columns(
        &query,
        &[],
        &ProductionControl::new(&denied, &token, &token),
        &mut |_| panic!("decode must be admitted before observation"),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(parent.used(), sibling.bytes());
}

#[test]
fn output_label_callbacks_stop_on_errors_both_cancellations_and_unwind() {
    for sql in ["SELECT lower('a'), upper('b')", "VALUES (1,2)"] {
        let query = query(sql);
        for action in 0..4 {
            let parent = MemoryBudget::new(2048);
            let sibling = parent.reserve(512).unwrap();
            let budget = parent.child(1024);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut calls = 0;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                visit_query_output_columns(
                    &query,
                    &[],
                    &ProductionControl::new(&budget, &original, &invoking),
                    &mut |_| {
                        calls += 1;
                        assert!(budget.used() > 0);
                        match action {
                            0 => original.cancel(),
                            1 => invoking.cancel(),
                            2 => return Err(SQLError::Internal("label visitor failed".into())),
                            _ => panic!("label visitor unwound"),
                        }
                        Ok(())
                    },
                )
            }));
            assert_eq!(calls, 1);
            match action {
                0 | 1 => assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014")),
                2 => assert_eq!(
                    result.unwrap().unwrap_err().to_string(),
                    "internal error: label visitor failed"
                ),
                _ => assert!(result.is_err()),
            }
            assert_eq!(budget.used(), 0);
            assert_eq!(parent.used(), sibling.bytes());
        }
    }
}

#[test]
fn absent_and_empty_output_descriptors_remain_distinct() {
    let mut query = query("VALUES (1)");
    let RelationalPlan::Values { rows, .. } = &mut query.root else {
        panic!("values");
    };
    rows.clear();
    let control = ProductionControl::uncontrolled();
    assert!(
        !visit_query_output_columns(&query, &[], &control, &mut |_| {
            panic!("absent descriptor")
        })
        .unwrap()
    );
    let RelationalPlan::Values { rows, .. } = &mut query.root else {
        panic!("values");
    };
    rows.push(vec![]);
    assert!(visit_query_output_columns(&query, &[], &control, &mut |_| {
        panic!("empty descriptor")
    })
    .unwrap());
}
