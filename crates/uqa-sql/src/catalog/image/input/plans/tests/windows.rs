//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image::input::window_tests::{assert_retirement, read_windowed, WINDOWS};

#[test]
fn native_query_windows_preserve_sources_mutations_and_original_arenas() {
    for sql in [
        "SELECT DISTINCT a + 1 AS x, (SELECT 7) FROM t LEFT JOIN u USING (a) WHERE t.a > 0 ORDER BY x DESC NULLS FIRST LIMIT 4 OFFSET 1",
        "SELECT sum(a) OVER (w ROWS BETWEEN 2 PRECEDING AND CURRENT ROW EXCLUDE TIES) FROM t WINDOW w AS (ORDER BY a)",
        "SELECT * FROM ROWS FROM (generate_series(1, 3), unnest(ARRAY[1, 2])) WITH ORDINALITY AS q(a, b, n)",
        "WITH RECURSIVE q(a) AS (VALUES (1) UNION ALL SELECT a + 1 FROM q WHERE a < 3) SEARCH DEPTH FIRST BY a SET path CYCLE a SET cycled TO true DEFAULT false USING trail SELECT * FROM q",
        "WITH q AS (INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a) SELECT * FROM q",
        "WITH q AS (UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a) SELECT * FROM q",
        "WITH q AS (DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a) SELECT * FROM q",
        "WITH q AS (MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN UPDATE SET b = u.b WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET b = 0 WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a) SELECT * FROM q",
    ] {
        let source = query(sql);
        let bytes = encode(&source);
        for (width, steps) in WINDOWS {
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let value = read_windowed(QueryPlan::image_input(&memory, &token).unwrap(), &bytes, width, steps);
            assert_eq!(serde_json::to_value(&*value).unwrap(), serde_json::to_value(&source).unwrap(), "{sql}");
            assert_eq!(encode(&value), bytes);
            token.cancel();
            let full = memory.reserve(memory.available()).unwrap();
            assert_eq!(encode(&value), bytes);
            drop((value, full));
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn native_query_windows_retire_original_parents_on_every_terminal_path() {
    let source = query("WITH q AS (UPDATE t SET a[1:2] = ARRAY[3, 4] FROM (SELECT 1 AS a) u WHERE t.a[1] = u.a RETURNING t.a) SELECT * FROM q UNION ALL SELECT ARRAY[5, 6]");
    assert_retirement(&encode(&source), QueryPlan::image_input);
}

#[test]
fn native_query_windows_preserve_large_fields_and_reject_original_scope_errors() {
    let source = query(&format!("SELECT '{}' AS original", "한λ😀".repeat(1025)));
    let bytes = encode(&source);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let value = read_windowed(
            QueryPlan::image_input(&memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&value), bytes);
        drop(value);
        assert_eq!(memory.used(), 0);
    }
    // Independent wire: a scalar address cannot refer to the empty VALUES arena.
    let mut scalar = vec![26];
    scalar.extend(0u64.to_le_bytes());
    let bytes = values_wire(&scalar);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let mut input = QueryPlan::image_input(&memory, &token).unwrap();
        let mut source = bytes.as_slice();
        loop {
            match input.advance(
                &mut source,
                uqa_core::image_io::ImageDecodeWindow {
                    bytes: width,
                    steps,
                },
            ) {
                Ok(None) => {}
                Ok(_) => panic!("invalid native query address was accepted"),
                Err(error) => {
                    assert!(error.to_string().contains("native query arena address"));
                    break;
                }
            }
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_query_windows_keep_deep_input_and_retirement_on_small_stacks() {
    let mut scalar = vec![14; 10000];
    scalar.push(0);
    let bytes = values_wire(&scalar);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let value = read_windowed(
                QueryPlan::image_input(&memory, &token).unwrap(),
                &bytes,
                17,
                3,
            );
            assert_eq!(encode(&value), bytes);
            let full = memory.reserve(memory.available()).unwrap();
            assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
            drop(full);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_expression_windows_keep_the_original_ordered_query_arena() {
    let mut bytes = b"UQAXPL01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(26);
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(2u64.to_le_bytes());
    bytes.extend_from_slice(&values_wire(&[0])[16..]);
    bytes.extend_from_slice(&values_wire(&[2])[16..]);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let value = read_windowed(
            ExpressionPlan::image_input(&memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(value.scalar, ScalarExpr::ScalarSubquery(1));
        assert_eq!(value.subqueries.len(), 2);
        let RelationalPlan::Values { rows, .. } = &value.subqueries[1].root else {
            panic!("values");
        };
        assert_eq!(rows[0][0], ScalarExpr::Default);
        let output_memory = MemoryBudget::new(16 << 20);
        assert_eq!(
            collect(value.image_reader(&output_memory, &token).unwrap()),
            bytes
        );
        drop(value);
        assert_eq!(memory.used(), 0);
        assert_eq!(output_memory.used(), 0);
    }
    assert_retirement(&bytes, ExpressionPlan::image_input);
}

#[test]
fn native_bound_plan_windows_keep_each_original_root_constructor() {
    for sql in [
        "SELECT (SELECT 1), sum(a) OVER (ORDER BY a) FROM t",
        "INSERT INTO t(a) VALUES (1) RETURNING a",
        "UPDATE t SET a = (SELECT 1) RETURNING a",
        "DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a",
        "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED THEN UPDATE SET b = u.b WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a",
    ] {
        let source = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let scratch = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let bytes = collect(source.image_reader(&scratch, &token).unwrap());
        for (width, steps) in WINDOWS {
            let memory = MemoryBudget::new(16 << 20);
            let value = read_windowed(UnifiedPlan::image_input(&memory, &token).unwrap(), &bytes, width, steps);
            assert_eq!(serde_json::to_value(&*value).unwrap(), serde_json::to_value(&source).unwrap());
            assert_eq!(collect(value.image_reader(&scratch, &token).unwrap()), bytes);
            drop(value);
            assert_eq!(memory.used(), 0);
        }
        assert_retirement(&bytes, UnifiedPlan::image_input);
        assert_eq!(scratch.used(), 0);
    }
}
