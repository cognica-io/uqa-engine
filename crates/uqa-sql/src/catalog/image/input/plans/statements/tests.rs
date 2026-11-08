//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
mod resources;

fn encode(value: &Statement) -> Vec<u8> {
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let mut reader = value.image_reader(&memory, &token).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 23];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(memory.used(), 0);
    bytes
}

fn check_image(value: &Statement) {
    let bytes = encode(value);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let restored = Statement::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(
        serde_json::to_value(&*restored).unwrap(),
        serde_json::to_value(value).unwrap()
    );
    assert_eq!(encode(&restored), bytes);
    assert!(memory.used() > 0);
    std::thread::spawn(move || drop(restored)).join().unwrap();
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_statement_input_preserves_queries_sources_windows_and_cte_bodies() {
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
        "SELECT (SELECT sum(a) OVER () FROM t) + 1 WHERE EXISTS (SELECT 1) AND 1 IN (SELECT 1)",
        "SELECT array_agg((SELECT 1) ORDER BY (SELECT 2) DESC NULLS LAST) FILTER (WHERE EXISTS (SELECT 3))",
    ] {
        check_image(&crate::compile(sql).unwrap().remove(0));
    }
}

#[test]
fn native_statement_input_preserves_mutations_and_notify() {
    for sql in [
        "INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a",
        "UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a",
        "DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a",
        "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN UPDATE SET b = u.b WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET b = 0 WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a",
        "NOTIFY events, 'hello'",
    ] {
        check_image(&crate::compile(sql).unwrap().remove(0));
    }
}

#[test]
fn native_statement_input_matches_independent_wire_and_rejects_every_partial_image() {
    let mut bytes = b"UQASTM01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(1);
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(2u64.to_le_bytes());
    bytes.extend([0, 2]);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let restored = Statement::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert!(
        matches!(&*restored, Statement::Values { rows } if matches!(&rows[..], [row] if matches!(&row[..], [Expr::Star, Expr::Default])))
    );
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    for sql in [
        "SELECT (SELECT 1), sum(a) OVER () FROM t",
        "WITH q AS (UPDATE t SET a = 1 RETURNING a) SELECT * FROM q",
        "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED THEN DELETE",
    ] {
        let bytes = encode(&crate::compile(sql).unwrap().remove(0));
        for end in 0..bytes.len() {
            assert!(
                Statement::read_image(&mut &bytes[..end], &memory, &token).is_err(),
                "{sql}: {end}"
            );
            assert_eq!(memory.used(), 0);
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(Statement::read_image(&mut trailing.as_slice(), &memory, &token).is_err());
        let denied = MemoryBudget::new(0);
        assert!(Statement::read_image(&mut bytes.as_slice(), &denied, &token).is_err());
        assert_eq!(denied.used(), 0);
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_statement_input_retires_deep_scalar_roots_without_stack_or_allocation() {
    let mut bytes = b"UQASTM01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(1);
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(std::iter::repeat_n(14, 10000));
    bytes.push(0);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let value = Statement::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
            let remaining = memory.reserve((64 << 20) - memory.used()).unwrap();
            assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
            drop(remaining);
            assert_eq!(memory.used(), 0);
            assert!(
                Statement::read_image(&mut &bytes[..bytes.len() - 1], &memory, &token).is_err()
            );
            assert_eq!(memory.used(), 0);
            let denied = MemoryBudget::new(64 << 10);
            assert!(Statement::read_image(&mut bytes.as_slice(), &denied, &token).is_err());
            assert_eq!(denied.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
