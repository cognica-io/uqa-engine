//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{FromClause, SelectStmt};
use uqa_core::memory::ProductionControl;

fn collect(mut reader: DefinitionImageReader<'_>, width: usize) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = vec![0; width];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= 16384);
        if count == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    output
}

fn encode(value: &Statement, width: usize) -> Vec<u8> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let image = collect(value.image_reader(&memory, &token).unwrap(), width);
    assert_eq!(memory.used(), 0);
    image
}

#[test]
fn native_statement_output_matches_independent_values_and_notify_wire() {
    let values = Statement::Values {
        rows: vec![vec![Expr::Star, Expr::Default]],
    };
    let mut expected = b"UQASTM01".to_vec();
    expected.extend(1u64.to_le_bytes());
    expected.push(1);
    expected.extend(1u64.to_le_bytes());
    expected.extend(2u64.to_le_bytes());
    expected.extend([0, 2]);
    for width in [1, 7, 16384, 32768] {
        assert_eq!(encode(&values, width), expected);
    }
    let value = Statement::Notify {
        channel: "a.\"b".into(),
        payload: "α\0data".into(),
    };
    let mut expected = b"UQASTM01".to_vec();
    expected.extend(1u64.to_le_bytes());
    expected.push(6);
    for text in ["a.\"b", "α\0data"] {
        expected.extend((text.len() as u64).to_le_bytes());
        expected.extend(text.as_bytes());
    }
    assert_eq!(encode(&value, 1), expected);
}

#[test]
fn native_statement_output_preserves_original_queries_mutations_and_stored_projection() {
    for sql in [
        "SELECT t.a AS named, (SELECT 1) FROM t LEFT JOIN u USING (a) WHERE t.a > 0 ORDER BY named LIMIT 3 OFFSET 1 FOR UPDATE OF t",
        "SELECT sum(a) OVER w FROM t WINDOW w AS (PARTITION BY b ORDER BY a ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)",
        "SELECT a, count(*) FROM t GROUP BY DISTINCT GROUPING SETS ((a), ()) HAVING count(*) > 0",
        "SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1 LIMIT 4",
        "VALUES (1, 'a'), (2, 'b')",
        "SELECT * FROM LATERAL (VALUES (1)) AS v(a), LATERAL (SELECT 2) AS q(b)",
        "SELECT * FROM ROWS FROM (generate_series(1, 3), unnest(ARRAY[1, 2])) WITH ORDINALITY AS q(a, b, n)",
        "WITH RECURSIVE q(a) AS (VALUES (1) UNION ALL SELECT a + 1 FROM q WHERE a < 3) SEARCH DEPTH FIRST BY a SET path CYCLE a SET cycled TO true DEFAULT false USING trail SELECT * FROM q",
        "WITH q AS (INSERT INTO t(a) VALUES (1) RETURNING a) SELECT * FROM q",
        "WITH q AS (UPDATE t SET a = 2 RETURNING a) SELECT * FROM q",
        "WITH q AS (DELETE FROM t RETURNING a) SELECT * FROM q",
        "WITH q AS (MERGE INTO t USING u ON t.a = u.a WHEN MATCHED THEN DELETE RETURNING t.a) SELECT * FROM q",
        "INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a",
        "UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a",
        "DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a",
        "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN UPDATE SET b = u.b WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET b = 0 WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a",
        "SELECT (SELECT sum(a) OVER () FROM t) + 1 WHERE EXISTS (SELECT 1) AND 1 IN (SELECT 1)",
    ] {
        let value = crate::compile(sql).unwrap().remove(0);
        let expected = encode(&value, 17);
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let stored = crate::plan::copy::capture_stored_with_control(&value, &control).unwrap();
        assert_eq!(encode(&stored, 1), expected, "{sql}");
        drop(stored);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_statement_output_bounds_borrowed_payload_scratch_and_terminal_failures() {
    let value = Statement::Notify {
        channel: "channel".into(),
        payload: "x".repeat(1 << 20),
    };
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let allocations = allocation_counter::measure(|| {
        let reader = value.image_reader(&memory, &token).unwrap();
        assert_eq!(memory.used(), 0);
        drop(reader);
    });
    assert_eq!(allocations.count_total, 0);
    let mut reader = value.image_reader(&memory, &token).unwrap();
    let mut buffer = vec![0; 32768];
    let mut length = 0;
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= 16384);
        assert!(memory.used() < 32768);
        length += count;
        if count == 0 {
            break;
        }
    }
    assert_eq!(length, 16 + 1 + 8 + 7 + 8 + (1 << 20));
    drop(reader);
    assert_eq!(memory.used(), 0);
    let denied = MemoryBudget::new(0);
    let mut reader = value.image_reader(&denied, &token).unwrap();
    assert!(reader.read(&mut buffer).is_err());
    assert!(reader.read(&mut buffer).is_err());
    assert_eq!(denied.used(), 0);
    let invalid = crate::compile("CREATE TABLE unrelated(a int)")
        .unwrap()
        .remove(0);
    assert!(invalid.image_reader(&memory, &token).is_err());
    let expression = Expr::ScalarSubquery(Box::new(empty_query()));
    let mut scalar = expression.image_reader(&memory, &token).unwrap();
    assert!(scalar.read(&mut buffer).is_err());
    assert_eq!(memory.used(), 0);
}

fn empty_query() -> SelectStmt {
    SelectStmt {
        windows: Vec::new(),
        projections: Vec::new(),
        values: Vec::new(),
        from: None,
        r#where: None,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        group_distinct: false,
        having: None,
        order_by: Vec::new(),
        limit: None,
        with_ties: false,
        offset: None,
        with: Vec::new(),
        set_op: None,
        distinct: false,
        distinct_on: Vec::new(),
        locking: Vec::new(),
    }
}

#[test]
fn native_statement_output_retains_cancellation_and_releases_unwinding_scratch() {
    let value = crate::compile("SELECT (SELECT 1) + 2").unwrap().remove(0);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut reader = value.image_reader(&memory, &token).unwrap();
    let mut buffer = [0; 17];
    reader.read(&mut buffer).unwrap();
    token.cancel();
    assert!(reader.read(&mut buffer).is_err());
    assert_eq!(memory.used(), 0);
    assert!(reader.read(&mut buffer).is_err());
    assert!(value.image_reader(&memory, &token).is_err());
    drop(reader);
    let token = CancellationToken::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut reader = value.image_reader(&memory, &token).unwrap();
        reader.read(&mut buffer).unwrap();
        panic!("consumer unwind");
    }));
    assert!(result.is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_statement_output_visits_deep_query_syntax_without_stack_growth() {
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(|| {
            let mut query = empty_query();
            for _ in 0..10000 {
                let source = FromClause::Subquery {
                    lateral: true,
                    body: Box::new(query),
                    alias: None,
                    column_aliases: Vec::new(),
                };
                query = empty_query();
                query.from = Some(source);
            }
            let value = Statement::Select(Box::new(query));
            let memory = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let mut reader = value.image_reader(&memory, &token).unwrap();
            let mut output = [0; 127];
            while reader.read(&mut output).unwrap() != 0 {}
            drop(reader);
            assert_eq!(memory.used(), 0);
            let Statement::Select(mut query) = value else {
                unreachable!()
            };
            while let Some(FromClause::Subquery { body, .. }) = query.from.take() {
                query = body;
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
