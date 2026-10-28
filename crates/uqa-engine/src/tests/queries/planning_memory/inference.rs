//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const INFERRED: &str = "SELECT a.id AS id FROM inference_a a, inference_b b, inference_c c WHERE (a.id=b.id AND b.id=c.id) OR (b.id=a.id AND a.id=c.id) ORDER BY a.id";

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_inferred_join_conditions_recover_and_preserve_postgresql_rows(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("inference-owner.db"));
    engine.sql("CREATE TABLE inference_a(id integer); CREATE TABLE inference_b(id integer); CREATE TABLE inference_c(id integer); INSERT INTO inference_a VALUES (1),(2),(3),(NULL); INSERT INTO inference_b VALUES (1),(2),(4),(NULL); INSERT INTO inference_c VALUES (1),(2),(5),(NULL)", &[]).unwrap();
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let input = UnifiedPlan::lower(uqa_sql::compile(INFERRED).unwrap().remove(0));
    let before = memory.used();
    let baseline =
        crate::capabilities::statement_planning::optimize_engine_plan(&engine, input.clone())
            .unwrap();
    assert!(
        baseline.reserved_bytes() > 0,
        "{provider:?}: inferred output has no lease"
    );
    assert_eq!(memory.used(), before + baseline.reserved_bytes());
    drop(baseline);
    assert_eq!(memory.used(), before);
    let occupied = memory.reserve(memory.available() - 512).unwrap();
    let allocation = allocation_counter::measure(|| {
        let error = crate::capabilities::statement_planning::optimize_engine_plan(&engine, input)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}");
    });
    assert!(
        allocation.bytes_total < 256 * 1024,
        "{provider:?}: {allocation:?}"
    );
    drop(occupied);
    assert_eq!(memory.used(), before);
    // Independently replayed on PostgreSQL 18.4: only non-NULL matching IDs 1 and 2 survive both branch forms.
    let expected = [1, 2]
        .map(|id| std::collections::BTreeMap::from([("id".into(), Value::Int(id))]))
        .to_vec();
    assert_eq!(
        engine.sql(INFERRED, &[]).unwrap().rows,
        expected,
        "{provider:?}"
    );
    engine
        .sql(&format!("PREPARE inferred_join AS {INFERRED}"), &[])
        .unwrap();
    assert_eq!(
        engine.sql("EXECUTE inferred_join", &[]).unwrap().rows,
        expected,
        "{provider:?}"
    );
    engine
        .sql(
            &format!("EXPLAIN (ANALYZE, MEMORY, FORMAT JSON) {INFERRED}"),
            &[],
        )
        .unwrap();
    let selected = engine.lookup_prepared("inferred_join").unwrap();
    assert!(
        selected.reserved_bytes() > 0,
        "{provider:?}: prepared output has no lease"
    );
    let mut copies = std::array::from_fn::<_, 64, _>(|_| None);
    let before = memory.used();
    let allocation = allocation_counter::measure(|| {
        for copy in &mut copies {
            *copy = Some(selected.clone());
        }
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(memory.used(), before);
    drop(copies);
    engine.sql("DEALLOCATE inferred_join", &[]).unwrap();
    assert_eq!(selected.strong_count(), 1);
    let before = memory.used();
    let reserved = selected.reserved_bytes();
    drop(selected);
    assert_eq!(memory.used(), before - reserved);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
