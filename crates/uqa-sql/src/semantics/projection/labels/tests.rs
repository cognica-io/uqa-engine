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

#[test]
fn retained_projection_labels_borrow_inputs_and_decode_once() {
    let mut query = query("SELECT a, q.b, 1 AS named, custom.\"a\"\"b\"(1), custom.\"a\"\"b\"(2)");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    block.projections[1].alias = None;
    let ScalarExpr::QualifiedColumn { column, .. } = &block.projections[1].expr else {
        panic!("qualified column");
    };
    let aliases = ["x".repeat(64 * 1024)];
    let pointers = [
        aliases[0].as_ptr(),
        column.as_ptr(),
        block.projections[2].alias.as_ref().unwrap().as_ptr(),
    ];
    let parent = MemoryBudget::new(8192);
    let sibling = parent.reserve(512).unwrap();
    let budget = parent.child(4096);
    let token = CancellationToken::new();
    let mut labels = None;
    let allocation = allocation_counter::measure(|| {
        labels = Some(
            projection_labels_with_control(
                &block.projections,
                &aliases,
                &ProductionControl::new(&budget, &token, &token),
            )
            .unwrap(),
        );
    });
    assert!(allocation.bytes_total < 2048, "{allocation:?}");
    assert_eq!(
        allocation.bytes_current,
        i64::try_from(budget.used()).unwrap()
    );
    let labels = labels.unwrap();
    let allocation = allocation_counter::measure(|| {
        for _ in 0..1024 {
            let mut names = labels.iter();
            assert_eq!(names.len(), 5);
            for pointer in pointers {
                assert_eq!(names.next().unwrap().as_ptr(), pointer);
            }
            assert_eq!(names.next(), Some("a\"b"));
            assert_eq!(names.next(), Some("a\"b"));
            assert_eq!(names.next(), None);
        }
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(parent.used(), sibling.bytes() + budget.used());
    drop(labels);
    assert_eq!(budget.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
}

#[test]
fn retained_projection_labels_release_descriptor_and_partial_decoding() {
    let query = query("SELECT custom.\"a\"\"b\"(1), custom.\"other.name\"(2), lower('a')");
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block");
    };
    let slots = std::mem::size_of::<Label<'_>>() * block.projections.len();
    let token = CancellationToken::new();
    let mut failed = 0;
    let mut completed = 0;
    for limit in [0, slots - 1, slots, slots + 32, slots + 128, 4096] {
        let parent = MemoryBudget::new(8192);
        let sibling = parent.reserve(512).unwrap();
        let budget = parent.child(limit);
        match projection_labels_with_control(
            &block.projections,
            &[],
            &ProductionControl::new(&budget, &token, &token),
        ) {
            Ok(labels) => {
                completed += 1;
                assert_eq!(
                    labels.iter().collect::<Vec<_>>(),
                    ["a\"b", "other.name", "lower"]
                );
                assert!(budget.used() > slots);
            }
            Err(error) => {
                failed += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
                if limit >= slots {
                    assert!(budget.peak() >= slots);
                }
            }
        }
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
    }
    assert!(failed >= 4);
    assert!(completed >= 1);
}

#[test]
fn retained_projection_labels_release_on_cancellation_and_unwind() {
    let query = query("SELECT custom.\"a\"\"b\"(1), lower('a')");
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block");
    };
    for action in 0..3 {
        let parent = MemoryBudget::new(8192);
        let sibling = parent.reserve(512).unwrap();
        let budget = parent.child(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let labels = projection_labels_with_control(&block.projections, &[], &control)?;
            assert_eq!(labels.iter().next(), Some("a\"b"));
            assert!(budget.used() > 0);
            match action {
                0 => original.cancel(),
                1 => invoking.cancel(),
                _ => panic!("label consumer unwound"),
            }
            control.check()?;
            Ok::<_, SQLError>(())
        }));
        if action == 2 {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
            let allocation = allocation_counter::measure(|| {
                let Err(error) = projection_labels_with_control(&block.projections, &[], &control)
                else {
                    panic!("cancelled descriptor");
                };
                assert_eq!(error.sqlstate(), Some("57014"));
            });
            assert!(allocation.bytes_total < 1024, "{allocation:?}");
        }
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
    }
}
