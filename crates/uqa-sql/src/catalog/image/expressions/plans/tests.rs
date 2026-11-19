//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod metadata;
mod resources;

fn encode(value: &QueryPlan) -> Vec<u8> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let image = collect(value.image_reader(&memory, &token).unwrap(), 127);
    assert_eq!(memory.used(), 0);
    image
}

fn query(sql: &str) -> QueryPlan {
    let crate::plan::UnifiedPlan::Query(query) =
        crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query fixture");
    };
    *query
}

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

#[test]
fn query_image_producer_matches_independent_values_wire() {
    let mut value = query("VALUES (1)");
    let crate::plan::RelationalPlan::Values { rows, .. } = &mut value.root else {
        panic!("values");
    };
    rows[0][0] = crate::ScalarExpr::Star;
    let mut expected = b"UQAQRY01".to_vec();
    expected.extend(1u64.to_le_bytes());
    expected.push(0); // Original relations_bound.
    expected.extend(0u64.to_le_bytes()); // No CTEs.
    expected.push(2); // VALUES constructor.
    expected.extend(1u64.to_le_bytes()); // One row.
    expected.extend(1u64.to_le_bytes()); // One expression.
    expected.push(0); // Native scalar Star.
    expected.extend(0u64.to_le_bytes()); // Empty query arena.
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for width in [1, 7, 16384, 32768] {
        let reader = value.image_reader(&memory, &token).unwrap();
        assert_eq!(memory.used(), 0);
        let actual =
            std::thread::scope(|scope| scope.spawn(move || collect(reader, width)).join().unwrap());
        assert_eq!(actual, expected);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn query_images_preserve_the_native_stored_projection_and_mutation_ctes() {
    use uqa_core::memory::ProductionControl;
    for sql in [
        "SELECT t.a AS named, (SELECT 1) FROM t LEFT JOIN u USING (a) WHERE t.a > 0 ORDER BY named LIMIT 3 OFFSET 1 FOR UPDATE OF t",
        "SELECT sum(a) OVER w FROM t WINDOW w AS (PARTITION BY b ORDER BY a ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)",
        "SELECT a, count(*) FROM t GROUP BY DISTINCT GROUPING SETS ((a), ()) HAVING count(*) > 0",
        "SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1 LIMIT 4",
        "VALUES (1, 'a'), (2, 'b')",
        "SELECT * FROM LATERAL (VALUES (1)) AS v(a), LATERAL (SELECT 2) AS q(b)",
        "SELECT * FROM ROWS FROM (generate_series(1, 3), unnest(ARRAY[1, 2])) WITH ORDINALITY AS q(a, b, n)",
        "WITH RECURSIVE q(a) AS (VALUES (1) UNION ALL SELECT a + 1 FROM q WHERE a < 3) SEARCH DEPTH FIRST BY a SET path CYCLE a SET cycled TO true DEFAULT false USING trail SELECT * FROM q",
        "WITH q AS (INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a) SELECT * FROM q",
        "WITH q AS (UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a) SELECT * FROM q",
        "WITH q AS (DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a) SELECT * FROM q",
        "WITH q AS (MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN UPDATE SET b = u.b WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET b = 0 WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a) SELECT * FROM q",
    ] {
        let value = query(sql);
        let expected = encode(&value);
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let stored = crate::plan::copy::capture_stored_with_control(&value, &control).unwrap();
        assert_eq!(encode(&stored), expected, "{sql}");
        drop(stored);
        assert_eq!(memory.used(), 0);
        assert_eq!(collect(value.image_reader(&memory, &token).unwrap(), 1), expected);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn expression_plan_images_keep_the_complete_native_query_arena() {
    let value = crate::plan::ExpressionPlan {
        scalar: crate::ScalarExpr::ScalarSubquery(0),
        subqueries: vec![query("VALUES (1)"), query("VALUES (2)")],
    };
    let mut expected = b"UQAXPL01".to_vec();
    expected.extend(1u64.to_le_bytes());
    expected.push(26); // ScalarSubquery constructor.
    expected.extend(0u64.to_le_bytes());
    expected.extend(2u64.to_le_bytes());
    for query in &value.subqueries {
        expected.extend_from_slice(&encode(query)[16..]);
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    assert_eq!(
        collect(value.image_reader(&memory, &token).unwrap(), 11),
        expected
    );
    assert_eq!(memory.used(), 0);
}
