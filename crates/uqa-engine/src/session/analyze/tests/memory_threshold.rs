//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, StatisticsRefresh};

fn analyzed_rows(engine: &Engine) -> Option<u64> {
    engine
        .column_stats_in_execution("growing", StatisticsRefresh::Maintained)
        .unwrap()
        .get("v")
        .map(|stats| stats.row_count)
}

#[test]
fn memory_statistics_wait_for_the_change_threshold_before_reanalyzing() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE growing (id INTEGER PRIMARY KEY, v INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO growing SELECT g, g FROM generate_series(1, 1000) AS g",
            &[],
        )
        .unwrap();
    // Missing statistics are collected when first planned.
    assert_eq!(analyzed_rows(&engine), Some(1000));
    // 100 changes stay below 50 + 10% of the 1000 analyzed rows.
    engine
        .sql(
            "INSERT INTO growing SELECT g, g FROM generate_series(1001, 1100) AS g",
            &[],
        )
        .unwrap();
    assert_eq!(analyzed_rows(&engine), Some(1000));
    // The next 100 changes cross it.
    engine
        .sql("UPDATE growing SET v = -v WHERE id BETWEEN 1 AND 100", &[])
        .unwrap();
    assert_eq!(analyzed_rows(&engine), Some(1100));
    // Rolled-back changes do not count toward the next analysis.
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("DELETE FROM growing WHERE id <= 600", &[])
        .unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(analyzed_rows(&engine), Some(1100));
    // An explicit request still reports the current rows after any write.
    engine.sql("DELETE FROM growing WHERE id = 1", &[]).unwrap();
    assert_eq!(analyzed_rows(&engine), Some(1100));
    assert_eq!(engine.column_stats("growing").unwrap()["v"].row_count, 1099);
    assert_eq!(analyzed_rows(&engine), Some(1099));
}
