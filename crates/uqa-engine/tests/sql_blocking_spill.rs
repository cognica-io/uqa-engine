//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end blocking-operator tests with a one-byte `work_mem` budget. Every
//! non-empty input row is larger than the budget, so success proves that the
//! SQL aggregate/window adapters use their disk-backed physical paths.

use uqa_core::Value;
use uqa_engine::Engine;

fn corpus() -> Engine {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();
    engine
        .sql(
            "CREATE TABLE spill_data (id INTEGER PRIMARY KEY, g INTEGER, v INTEGER)",
            &[],
        )
        .unwrap();
    for id in 0..120_i64 {
        engine
            .sql(
                &format!(
                    "INSERT INTO spill_data (id, g, v) VALUES ({id}, {}, {})",
                    id % 3,
                    id % 10
                ),
                &[],
            )
            .unwrap();
    }
    engine
}

#[test]
fn tiny_work_mem_spills_group_map_distinct_and_ordered_collection_state() {
    let engine = corpus();
    let result = engine
        .sql(
            "SELECT g,
                    count(*) AS total,
                    count(DISTINCT v) AS unique_values,
                    array_agg(v ORDER BY v DESC) AS ordered_values
             FROM spill_data
             GROUP BY g
             ORDER BY g",
            &[],
        )
        .unwrap();

    assert_eq!(result.rows.len(), 3);
    for (group, row) in result.rows.iter().enumerate() {
        assert_eq!(row.get("g"), Some(&Value::Int(group as i64)));
        assert_eq!(row.get("total"), Some(&Value::Int(40)));
        assert_eq!(row.get("unique_values"), Some(&Value::Int(10)));
        let Some(Value::List(values)) = row.get("ordered_values") else {
            panic!("ordered array aggregate missing: {row:?}");
        };
        assert_eq!(values.len(), 40);
        assert!(values.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    let grouping_sets = engine
        .sql(
            "SELECT g, v, count(*) AS total
             FROM spill_data
             GROUP BY GROUPING SETS ((g), (v), ())",
            &[],
        )
        .unwrap();
    assert_eq!(
        grouping_sets.rows.len(),
        14,
        "unexpected grouping-set rows: {:?}",
        grouping_sets.rows
    );
    assert!(grouping_sets.rows.iter().any(|row| {
        row.get("g") == Some(&Value::Null)
            && row.get("v") == Some(&Value::Null)
            && row.get("total") == Some(&Value::Int(120))
    }));
}

#[test]
fn tiny_work_mem_spills_window_input_sort_and_random_access_partition() {
    let engine = corpus();
    let result = engine
        .sql(
            "SELECT id, v,
                    row_number() OVER (PARTITION BY g ORDER BY id) AS row_number,
                    lag(id, 1, -1) OVER (PARTITION BY g ORDER BY id) AS previous_id,
                    lead(id, 1, -1) OVER (PARTITION BY g ORDER BY id) AS next_id,
                    ntile(7) OVER (PARTITION BY g ORDER BY id) AS tile,
                    sum(v) OVER (
                        PARTITION BY g ORDER BY id
                        ROWS BETWEEN 2 PRECEDING AND CURRENT ROW
                    ) AS rolling_sum
             FROM spill_data
             WHERE g = 0
             ORDER BY id",
            &[],
        )
        .unwrap();

    assert_eq!(result.rows.len(), 40);
    for (index, row) in result.rows.iter().enumerate() {
        let id = (index * 3) as i64;
        assert_eq!(row.get("id"), Some(&Value::Int(id)));
        assert_eq!(row.get("row_number"), Some(&Value::Int(index as i64 + 1)));
        assert_eq!(
            row.get("previous_id"),
            Some(&Value::Int(if index == 0 { -1 } else { id - 3 }))
        );
        assert_eq!(
            row.get("next_id"),
            Some(&Value::Int(if index == 39 { -1 } else { id + 3 }))
        );
        let start = index.saturating_sub(2);
        let expected_sum = (start..=index).map(|item| ((item * 3) % 10) as i64).sum();
        assert_eq!(row.get("rolling_sum"), Some(&Value::Int(expected_sum)));
        let Some(Value::Int(tile)) = row.get("tile") else {
            panic!("ntile output missing: {row:?}");
        };
        assert!((1..=7).contains(tile));
    }

    let huge_following = engine
        .sql(
            "SELECT id,
                    sum(v) OVER (
                        ORDER BY id
                        ROWS BETWEEN CURRENT ROW AND 9223372036854775807 FOLLOWING
                    ) AS suffix_sum
             FROM spill_data
             WHERE g = 0
             ORDER BY id",
            &[],
        )
        .unwrap();
    let expected_total: i64 = (0..40).map(|index| i64::from((index * 3) % 10)).sum();
    assert_eq!(
        huge_following.rows[0].get("suffix_sum"),
        Some(&Value::Int(expected_total))
    );
    assert_eq!(
        huge_following.rows[39].get("suffix_sum"),
        Some(&Value::Int(7))
    );
}

#[test]
fn tiny_work_mem_streams_set_children_and_distinct_before_final_rows() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();

    let union = engine
        .sql(
            "SELECT generate_series(1, 2048) AS value
             UNION
             SELECT generate_series(1025, 3072) AS value
             ORDER BY value",
            &[],
        )
        .unwrap();
    assert_eq!(union.rows.len(), 3_072);
    assert_eq!(union.rows[0].get("value"), Some(&Value::Int(1)));
    assert_eq!(union.rows[3_071].get("value"), Some(&Value::Int(3_072)));

    let intersect = engine
        .sql(
            "SELECT generate_series(1, 2048) AS value
             INTERSECT
             SELECT generate_series(1025, 3072) AS value",
            &[],
        )
        .unwrap();
    assert_eq!(intersect.rows.len(), 1_024);

    let distinct_limit = engine
        .sql(
            "SELECT DISTINCT generate_series(1, 2048) AS value
             ORDER BY value LIMIT 4 OFFSET 2",
            &[],
        )
        .unwrap();
    let values = distinct_limit
        .rows
        .iter()
        .map(|row| row.get("value").cloned().unwrap_or(Value::Null))
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        vec![Value::Int(3), Value::Int(4), Value::Int(5), Value::Int(6)]
    );
}

#[test]
fn tiny_work_mem_bounds_recursive_cte_working_accumulated_and_dedup_state() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();

    let deduplicated = engine
        .sql(
            "WITH RECURSIVE numbers(n) AS (
                 SELECT generate_series(1, 3072)
                 UNION
                 SELECT n FROM numbers
             )
             SELECT count(*) AS total FROM numbers",
            &[],
        )
        .unwrap();
    assert_eq!(deduplicated.rows[0].get("total"), Some(&Value::Int(3_072)));

    let accumulated = engine
        .sql(
            "WITH RECURSIVE numbers(n) AS (
                 SELECT 1
                 UNION ALL
                 SELECT n + 1 FROM numbers WHERE n < 900
             )
             SELECT count(*) AS total, max(n) AS maximum FROM numbers",
            &[],
        )
        .unwrap();
    assert_eq!(accumulated.rows[0].get("total"), Some(&Value::Int(900)));
    assert_eq!(accumulated.rows[0].get("maximum"), Some(&Value::Int(900)));
}

#[test]
fn tiny_work_mem_bounds_high_cardinality_facet_aggregation() {
    let engine = corpus();
    let facets = engine
        .sql("SELECT uqa_facets(id) FROM spill_data", &[])
        .unwrap();
    assert_eq!(facets.rows.len(), 120);
    assert!(facets
        .rows
        .iter()
        .all(|row| row.get("facet_count") == Some(&Value::Int(1))));
    assert_eq!(facets.rows[0].get("facet_value"), Some(&Value::Int(0)));
    assert_eq!(facets.rows[119].get("facet_value"), Some(&Value::Int(119)));
}
