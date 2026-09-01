//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn cursor_pair(engine: &Engine, sql: &str) -> (i64, i64) {
    let result = engine.sql(sql, &[]).unwrap();
    (
        integer_column(&result, "value")[0],
        integer_column(&result, "observed")[0],
    )
}

fn sequence_value(engine: &Engine, name: &str) -> i64 {
    integer_column(
        &engine
            .sql(&format!("SELECT currval('{name}') AS value"), &[])
            .unwrap(),
        "value",
    )[0]
}

#[test]
fn pg18_scroll_cursor_reevaluates_target_expressions_in_scan_direction() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE directional_target_sequence; BEGIN", &[])
        .unwrap();
    engine
        .sql(
            "DECLARE directional_cursor SCROLL CURSOR FOR SELECT value, nextval('directional_target_sequence') AS observed FROM generate_series(1, 4) AS values(value)",
            &[],
        )
        .unwrap();

    assert_eq!(
        cursor_pair(&engine, "FETCH NEXT FROM directional_cursor"),
        (1, 1)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH NEXT FROM directional_cursor"),
        (2, 2)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH PRIOR FROM directional_cursor"),
        (1, 3)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH NEXT FROM directional_cursor"),
        (2, 4)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH FORWARD 0 FROM directional_cursor"),
        (2, 6)
    );
    assert_eq!(
        engine
            .sql("MOVE FORWARD 0 FROM directional_cursor", &[])
            .unwrap()
            .affected_rows,
        1
    );
    assert_eq!(sequence_value(&engine, "directional_target_sequence"), 6);
    assert_eq!(
        cursor_pair(&engine, "FETCH ABSOLUTE 1 FROM directional_cursor"),
        (1, 7)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH RELATIVE 2 FROM directional_cursor"),
        (3, 9)
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_scroll_cursor_rechecks_volatile_filters_while_revisiting_rows() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE directional_filter_sequence; CREATE SEQUENCE directional_projection_sequence; BEGIN",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "DECLARE filtered_cursor SCROLL CURSOR FOR SELECT value, nextval('directional_projection_sequence') AS observed FROM generate_series(1, 5) AS values(value) WHERE nextval('directional_filter_sequence') % 2 = 0",
            &[],
        )
        .unwrap();

    assert_eq!(
        cursor_pair(&engine, "FETCH NEXT FROM filtered_cursor"),
        (2, 1)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH NEXT FROM filtered_cursor"),
        (4, 2)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH PRIOR FROM filtered_cursor"),
        (2, 3)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH NEXT FROM filtered_cursor"),
        (4, 4)
    );
    assert_eq!(sequence_value(&engine, "directional_filter_sequence"), 8);
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_scroll_cursor_preserves_limit_and_sort_directional_boundaries() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE directional_slice_sequence; BEGIN", &[])
        .unwrap();
    engine
        .sql(
            "DECLARE slice_cursor SCROLL CURSOR FOR SELECT value, nextval('directional_slice_sequence') AS observed FROM generate_series(1, 5) AS values(value) OFFSET 2 LIMIT 2",
            &[],
        )
        .unwrap();
    assert_eq!(cursor_pair(&engine, "FETCH NEXT FROM slice_cursor"), (3, 3));
    assert_eq!(cursor_pair(&engine, "FETCH NEXT FROM slice_cursor"), (4, 4));
    assert!(engine
        .sql("FETCH NEXT FROM slice_cursor", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        cursor_pair(&engine, "FETCH PRIOR FROM slice_cursor"),
        (4, 4)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH PRIOR FROM slice_cursor"),
        (3, 5)
    );
    assert_eq!(cursor_pair(&engine, "FETCH NEXT FROM slice_cursor"), (4, 6));
    engine.sql("ROLLBACK", &[]).unwrap();

    engine
        .sql("CREATE SEQUENCE directional_order_sequence; BEGIN", &[])
        .unwrap();
    engine
        .sql(
            "DECLARE ordered_cursor SCROLL CURSOR FOR SELECT value, nextval('directional_order_sequence') AS observed FROM generate_series(1, 4) AS values(value) ORDER BY value DESC",
            &[],
        )
        .unwrap();
    assert_eq!(
        cursor_pair(&engine, "FETCH NEXT FROM ordered_cursor"),
        (4, 1)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH LAST FROM ordered_cursor"),
        (1, 5)
    );
    assert_eq!(
        cursor_pair(&engine, "FETCH FIRST FROM ordered_cursor"),
        (4, 6)
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_scroll_cursor_materializes_plans_without_backwards_execution() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE directional_join_sequence; BEGIN", &[])
        .unwrap();
    engine
        .sql(
            "DECLARE joined_cursor SCROLL CURSOR FOR SELECT left_value, right_value, nextval('directional_join_sequence') AS observed FROM generate_series(1, 2) AS left_values(left_value) CROSS JOIN generate_series(1, 2) AS right_values(right_value)",
            &[],
        )
        .unwrap();
    let first = engine.sql("FETCH NEXT FROM joined_cursor", &[]).unwrap();
    let first_observed = integer_column(&first, "observed")[0];
    engine.sql("FETCH NEXT FROM joined_cursor", &[]).unwrap();
    let prior = engine.sql("FETCH PRIOR FROM joined_cursor", &[]).unwrap();
    assert_eq!(integer_column(&prior, "observed"), [first_observed]);
    assert_eq!(sequence_value(&engine, "directional_join_sequence"), 2);
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_scroll_cursor_reevaluates_values_rows_when_scanning_backwards() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE directional_values_sequence; BEGIN", &[])
        .unwrap();
    engine
        .sql(
            "DECLARE values_cursor SCROLL CURSOR FOR VALUES (1, nextval('directional_values_sequence')), (2, nextval('directional_values_sequence'))",
            &[],
        )
        .unwrap();
    for (sql, expected) in [
        ("FETCH NEXT FROM values_cursor", (1, 1)),
        ("FETCH NEXT FROM values_cursor", (2, 2)),
        ("FETCH PRIOR FROM values_cursor", (1, 3)),
    ] {
        let row = engine.sql(sql, &[]).unwrap();
        assert_eq!(
            (
                integer_column(&row, "column1")[0],
                integer_column(&row, "column2")[0],
            ),
            expected
        );
    }
    engine.sql("ROLLBACK", &[]).unwrap();
}
