//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::RelationalPlan;
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
fn scoped_reference_observation_does_not_construct_empty_descriptors() {
    let mut nested = query("SELECT external_column");
    for _ in 0..64 {
        let mut outer = query("SELECT 1");
        let RelationalPlan::QueryBlock(block) = &mut outer.root else {
            panic!("block");
        };
        block.projections[0].expr = ScalarExpr::ScalarSubquery(0);
        block.subqueries.push(nested);
        nested = outer;
    }
    let mut calls = 0;
    let allocation = allocation_counter::measure(|| {
        let mut visitor = |part: &ScalarExpr| {
            let ScalarExpr::Column(name) = part else {
                panic!("external reference");
            };
            assert_eq!(name.as_str(), "external_column");
            calls += 1;
        };
        let mut references = ScopedReferences::new(&mut visitor);
        references.query(&nested);
        assert!(references.is_complete());
    });
    assert_eq!(calls, 1);
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
}

fn lateral_query() -> QueryPlan {
    let mut query = query("SELECT l.v, r.w FROM (SELECT external_left) l CROSS JOIN LATERAL (SELECT l.v, external_right) r");
    let left = RowSchema::with_qualified_types(
        "l",
        vec!["v".into()],
        vec![Some(crate::ColumnType::Integer)],
    );
    let right = RowSchema::with_qualified_types(
        "r",
        vec!["w".into()],
        vec![Some(crate::ColumnType::Integer)],
    );
    query.analyzed_source = Some(std::sync::Arc::new(AnalyzedSource {
        schema: RowSchema::join(&left, &right, std::iter::empty()),
        children: Some(Box::new((
            AnalyzedSource {
                schema: left,
                children: None,
            },
            AnalyzedSource {
                schema: right,
                children: None,
            },
        ))),
    }));
    query
}

#[test]
fn controlled_scoped_references_borrow_lateral_layouts_without_an_allowance() {
    let query = lateral_query();
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let mut calls = 0;
    let allocation = allocation_counter::measure(|| {
        let mut visit = |part: &ScalarExpr| {
            let ScalarExpr::Column(name) = part else {
                panic!("external input");
            };
            assert_eq!(name.as_str(), ["external_left", "external_right"][calls]);
            calls += 1;
            Ok(())
        };
        let mut references = ControlledScopedReferences::new(
            &mut visit,
            ProductionControl::new(&memory, &token, &token),
        );
        references.query(&query).unwrap();
        assert!(references.is_complete());
    });
    assert_eq!(calls, 2);
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    assert_eq!(memory.peak(), 0);
}

#[test]
fn controlled_scoped_references_keep_missing_layouts_stars_and_nested_arenas() {
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for (sql, complete, expected) in [
        ("SELECT *", true, vec![]),
        ("SELECT (SELECT * FROM missing)", false, vec![]),
        (
            "SELECT (SELECT external_column)",
            true,
            vec!["external_column"],
        ),
        ("VALUES (external_column)", true, vec!["external_column"]),
    ] {
        let plan = query(sql);
        let mut actual = Vec::new();
        let mut visit = |part: &ScalarExpr| {
            if let ScalarExpr::Column(name) = part {
                actual.push(name.to_string());
            }
            Ok(())
        };
        let mut references = ControlledScopedReferences::new(&mut visit, control);
        references.query(&plan).unwrap();
        assert_eq!(references.is_complete(), complete, "{sql}");
        assert_eq!(actual, expected, "{sql}");
    }
    let mut visit = |_: &ScalarExpr| panic!("absent arena cannot produce an input reference");
    let mut references = ControlledScopedReferences::new(&mut visit, control);
    references
        .expression(&ScalarExpr::ScalarSubquery(3), &[])
        .unwrap();
    assert!(!references.is_complete());
    assert_eq!(memory.used(), 0);
}

#[test]
fn controlled_scoped_reference_callbacks_stop_on_failure_both_cancellations_and_unwind() {
    let query = lateral_query();
    for action in 0..4 {
        let parent = MemoryBudget::new(512);
        let sibling = parent.reserve(512).unwrap();
        let memory = parent.child(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut calls = 0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut visit = |_: &ScalarExpr| {
                calls += 1;
                match action {
                    0 => return Err(SQLError::Internal("reference observation failed".into())),
                    1 => original.cancel(),
                    2 => invoking.cancel(),
                    _ => panic!("reference observation unwound"),
                }
                Ok(())
            };
            ControlledScopedReferences::new(
                &mut visit,
                ProductionControl::new(&memory, &original, &invoking),
            )
            .query(&query)
        }));
        assert_eq!(calls, 1);
        match action {
            0 => assert_eq!(
                result.unwrap().unwrap_err().to_string(),
                "internal error: reference observation failed"
            ),
            1 | 2 => assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014")),
            _ => assert!(result.is_err()),
        }
        assert_eq!(memory.peak(), 0);
        assert_eq!(parent.used(), sibling.bytes());
    }
}

#[test]
fn controlled_query_inputs_admit_decoded_order_labels_and_preserve_output_precedence() {
    let plan = query(
        "SELECT custom.\"a\"\"b\"(x), y AS kept FROM source ORDER BY \"a\"\"b\", kept, other",
    );
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        panic!("block");
    };
    let token = CancellationToken::new();
    let denied = MemoryBudget::new(0);
    let mut count = 0;
    let error = visit_query_inputs_with_control(
        block,
        &ProductionControl::new(&denied, &token, &token),
        &mut |_| {
            count += 1;
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(count, 2, "projection roots precede ordering-label decoding");
    assert_eq!(denied.used(), 0);
    let memory = MemoryBudget::new(1024);
    let expected: Vec<_> = query_inputs(block).map(std::ptr::from_ref).collect();
    let mut actual = Vec::new();
    visit_query_inputs_with_control(
        block,
        &ProductionControl::new(&memory, &token, &token),
        &mut |part| {
            actual.push(std::ptr::from_ref(part));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 3);
    assert_eq!(memory.used(), 0);
}
