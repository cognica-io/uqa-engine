//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{exec, Engine, Value};

#[test]
fn explicit_analyze_bounds_exact_population_and_publishes_complete_statistics() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE analyzed(val INTEGER, category TEXT);
         INSERT INTO analyzed SELECT n,CASE WHEN n<=24576 THEN 'common' ELSE 'rare' END FROM generate_series(1,32768) AS s(n);
         SET work_mem='64kB'",
    );
    let allocated = allocation_counter::measure(|| exec(&engine, "ANALYZE analyzed"));
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
    let statistics = engine.column_stats("analyzed").unwrap();
    let value = &statistics["val"];
    assert_eq!(value.row_count, 32_768);
    assert_eq!(value.null_count, 0);
    assert_eq!(value.distinct_count, 32_768);
    assert_eq!(value.min_value, Some(Value::Int(1)));
    assert_eq!(value.max_value, Some(Value::Int(32_768)));
    let mut histogram = vec![Value::Int(1)];
    histogram.extend((1..100).map(|bucket| Value::Int(bucket * 32_768 / 100 + 1)));
    histogram.push(Value::Int(32_768));
    assert_eq!(value.histogram, histogram);
    assert!(value.mcv_values.is_empty());
    let category = &statistics["category"];
    assert_eq!(category.distinct_count, 2);
    assert_eq!(category.row_count, 32_768);
    assert_eq!(
        category.histogram,
        vec![Value::Str("common".into()), Value::Str("rare".into())]
    );
    assert_eq!(category.mcv_values, vec![Value::Str("common".into())]);
    assert_eq!(category.mcv_frequencies, vec![0.75]);
}
