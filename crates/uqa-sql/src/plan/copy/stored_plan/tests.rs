//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{catalog::work::inline, plan::copy::capture_stored_with_footprint};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

#[derive(Debug, Default)]
struct Windows {
    count: usize,
    allocations: u64,
    bytes: u64,
}

fn verify(source: &UnifiedPlan, limit: usize) -> Windows {
    let memory = MemoryBudget::new(limit);
    let reference = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let (expected, expected_children) =
        capture_stored_with_footprint(source, &ProductionControl::new(&reference, &token, &token))
            .unwrap();
    let mut driver = construction::Driver::new(control).unwrap();
    let output = driver
        .work()
        .submit(|work| capture(source, control, work))
        .unwrap();
    let mut progress = Windows::default();
    let mut complete = false;
    while !complete {
        let allocation = allocation_counter::measure(|| {
            for _ in 0..32 {
                if !inline(driver.advance()).unwrap() {
                    complete = true;
                    break;
                }
            }
        });
        progress.count += 1;
        progress.allocations = progress.allocations.max(allocation.count_total);
        progress.bytes = progress.bytes.max(allocation.bytes_total);
    }
    let (actual, actual_children) = inline(construction::receive(output)).unwrap();
    let output = serde_json::to_value(&*actual).unwrap();
    assert_eq!(output, serde_json::to_value(&*expected).unwrap());
    // These source plans have persistent fields only, so the input supplies a separate value oracle.
    assert_eq!(output, serde_json::to_value(source).unwrap());
    assert_eq!(
        (actual.reserved_bytes(), actual_children),
        (expected.reserved_bytes(), expected_children)
    );
    drop((driver, actual, expected));
    assert_eq!((memory.used(), reference.used()), (0, 0));
    progress
}

#[test]
fn bound_routine_projection_preserves_native_query_and_mutation_fields() {
    for sql in [
        "WITH d AS (DELETE FROM t RETURNING a) SELECT sum(a) OVER w FROM d WINDOW w AS (ORDER BY a ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)",
        "SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1 LIMIT 4",
        "INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a",
        "UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a",
        "DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a",
        "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN UPDATE SET b = u.b WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET b = 0 WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a",
    ] {
        verify(&plan(sql), 2 << 20);
    }
}

#[test]
fn bound_routine_projection_copies_values_at_the_original_nested_source_and_mutation_slots() {
    verify(&plan("VALUES (1)"), 16 << 10);
    for width in [2, 4096] {
        let rows = std::iter::repeat_n("(1)", width)
            .collect::<Vec<_>>()
            .join(",");
        for sql in [
            format!("SELECT count(*) FROM (VALUES {rows}) AS t(v)"),
            format!("INSERT INTO t(a) VALUES {rows}"),
        ] {
            let progress = verify(&plan(&sql), 8 << 20);
            eprintln!("bound projection {width} rows {progress:?}");
            if width > 2 {
                assert!(progress.count >= 128, "{progress:?}");
                assert!(
                    progress.allocations < 128 && progress.bytes < 256 << 10,
                    "{progress:?}"
                );
            }
        }
    }
}

#[test]
fn bound_routine_projection_bounds_wide_mutation_assignments_and_merge_branches() {
    for (columns, branches) in [(2, 2), (1024, 4096)] {
        let assignments = (0..columns)
            .map(|index| format!("c{index}=1"))
            .collect::<Vec<_>>()
            .join(",");
        let clauses = (0..branches)
            .map(|index| format!("WHEN MATCHED AND u.c0={index} THEN DO NOTHING"))
            .collect::<Vec<_>>()
            .join(" ");
        for (sql, count, slot) in [
            (
                format!("UPDATE t SET {assignments}"),
                columns,
                size_of::<crate::plan::AssignmentPlan>(),
            ),
            (
                format!("MERGE INTO t USING u ON true {clauses}"),
                branches,
                size_of::<crate::plan::MergeWhenPlan>(),
            ),
        ] {
            let progress = verify(&plan(&sql), 16 << 20);
            eprintln!("bound mutation {count} fields of {slot} bytes {progress:?}");
            if count > 2 {
                assert!(progress.count >= count / 32, "{progress:?}");
                // The final vector admits its full contiguous slot allocation before populating any element.
                assert!(
                    progress.allocations < 128
                        && progress.bytes < (slot * count + (128 << 10)) as u64,
                    "{progress:?}"
                );
            }
        }
    }
}

#[test]
fn bound_routine_projection_releases_original_literals_and_partial_fields_on_stop() {
    let mut source = plan("VALUES (1), (2), (3)");
    let UnifiedPlan::Query(query) = &mut source else {
        unreachable!()
    };
    let crate::plan::RelationalPlan::Values { rows, .. } = &mut query.root else {
        unreachable!()
    };
    let original = MemoryBudget::new(2 << 20);
    let original_token = CancellationToken::new();
    let retained = ProductionControl::new(&original, &original_token, &original_token);
    let value = uqa_core::Value::Str("λ".repeat(128 << 10));
    let (literal, _) = crate::ir::RetainedScalarLiteral::from_produced(
        retained.copy_value(&value).unwrap(),
        None,
        None,
        Some(950),
        &retained,
    )
    .unwrap();
    rows[1][0] = crate::ScalarExpr::RetainedLiteral(literal);
    let progress = verify(&source, 2 << 20);
    assert!(progress.count > 1, "{progress:?}");
    for mode in 0..4 {
        let first = CancellationToken::new();
        let second = CancellationToken::new();
        let memory = MemoryBudget::new(if mode == 3 { 128 << 10 } else { 2 << 20 });
        let control = ProductionControl::new(&memory, &first, &second);
        let mut driver = construction::Driver::new(control).unwrap();
        let output = driver
            .work()
            .submit(|work| capture(&source, control, work))
            .unwrap();
        if mode != 3 {
            while memory.used() < 256 << 10 {
                assert!(inline(driver.advance()).unwrap());
            }
            // The destination string now owns its reservation, and another chunk fits without publishing it.
            assert!(inline(driver.advance()).unwrap());
        }
        if mode == 1 {
            first.cancel();
        } else if mode == 2 {
            second.cancel();
        }
        if mode == 1 || mode == 2 {
            assert!(inline(driver.advance()).is_err());
        } else if mode == 3 {
            while inline(driver.advance()).unwrap() {}
            let error = inline(construction::receive(output.clone())).unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
        }
        drop((driver, output));
        assert_eq!(memory.used(), 0);
    }
    drop(source);
    assert_eq!(original.used(), 0);
}
