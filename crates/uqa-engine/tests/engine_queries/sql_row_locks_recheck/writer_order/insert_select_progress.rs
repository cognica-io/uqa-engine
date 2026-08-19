//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn register_visible_count(root: &Arc<Engine>) {
    let count_engine = Arc::downgrade(root);
    root.register_scalar_function_with_options(
        "insert_select_visible_count",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |args: &[Value]| {
            let engine = count_engine.upgrade().ok_or_else(|| {
                uqa_sql::SQLError::Internal(
                    "INSERT SELECT command-progress engine was dropped".into(),
                )
            })?;
            let Some(Value::Str(table)) = args.first() else {
                return Err(uqa_sql::SQLError::TypeMismatch(
                    "insert_select_visible_count expects one table name".into(),
                ));
            };
            Ok(Value::Int(
                i64::try_from(engine.table_doc_ids(table)?.len()).unwrap(),
            ))
        },
    )
    .unwrap();
}

fn register_visible_sum(root: &Arc<Engine>) {
    let sum_engine = Arc::downgrade(root);
    root.register_scalar_function_with_options(
        "command_progress_visible_sum",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |args: &[Value]| {
            let engine = sum_engine.upgrade().ok_or_else(|| {
                uqa_sql::SQLError::Internal("DML command-progress engine was dropped".into())
            })?;
            let Some(Value::Str(table)) = args.first() else {
                return Err(uqa_sql::SQLError::TypeMismatch(
                    "command_progress_visible_sum expects one table name".into(),
                ));
            };
            let mut sum = 0_i64;
            for doc_id in engine.table_doc_ids(table)? {
                if let Some(Value::Int(value)) = engine
                    .get_document(table, doc_id)?
                    .and_then(|document| document.get("seen").cloned())
                {
                    sum += value;
                }
            }
            Ok(Value::Int(sum))
        },
    )
    .unwrap();
}

fn seen(root: &Engine, table: &str) -> Vec<Value> {
    root.sql(&format!("SELECT seen FROM {table} ORDER BY id"), &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["seen"].clone())
        .collect()
}

fn sorted_seen(root: &Engine, table: &str) -> Vec<Value> {
    root.sql(&format!("SELECT seen FROM {table} ORDER BY seen"), &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["seen"].clone())
        .collect()
}

#[test]
fn insert_select_streams_command_progress_at_postgresql_execution_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let root = Arc::new(Engine::open(&directory.path().join("insert-select-progress.db")).unwrap());
    register_visible_count(&root);
    root.sql("CREATE TABLE progress_source (id INTEGER PRIMARY KEY); INSERT INTO progress_source VALUES (1), (2), (3); CREATE TABLE progress_ordered (id INTEGER PRIMARY KEY, seen INTEGER, snapshot_count INTEGER); CREATE TABLE progress_filter (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_sort_key (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_derived (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_srf (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_srf_limit (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_aggregate (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_window (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_union_all (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_union_distinct (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_union_limit (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_union_offset (id INTEGER PRIMARY KEY, seen INTEGER)", &[]).unwrap();

    root.sql("INSERT INTO progress_ordered SELECT id, insert_select_visible_count('progress_ordered'), (SELECT count(*) FROM progress_ordered) FROM progress_source ORDER BY id", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_ordered"),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );
    assert!(root
        .sql("SELECT snapshot_count FROM progress_ordered", &[])
        .unwrap()
        .rows
        .iter()
        .all(|row| row["snapshot_count"] == Value::Int(0)));

    root.sql("INSERT INTO progress_filter SELECT id, insert_select_visible_count('progress_filter') FROM progress_source WHERE insert_select_visible_count('progress_filter') = id - 1", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_filter"),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );

    root.sql("INSERT INTO progress_sort_key SELECT id, insert_select_visible_count('progress_sort_key') FROM progress_source ORDER BY insert_select_visible_count('progress_sort_key'), id", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_sort_key"),
        vec![Value::Int(0), Value::Int(0), Value::Int(0)]
    );

    root.sql("INSERT INTO progress_derived SELECT source_row.id, source_row.seen FROM (SELECT id, insert_select_visible_count('progress_derived') AS seen FROM progress_source ORDER BY id) AS source_row", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_derived"),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );

    root.sql("INSERT INTO progress_srf SELECT id * 10 + generate_series(1, 2), insert_select_visible_count('progress_srf') FROM progress_source", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_srf"),
        vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]
    );

    root.sql("INSERT INTO progress_srf_limit SELECT id * 10 + generate_series(1, 2), insert_select_visible_count('progress_srf_limit') FROM progress_source ORDER BY id LIMIT 3", &[]).unwrap();
    assert_eq!(
        root.sql("SELECT id FROM progress_srf_limit ORDER BY id", &[])
            .unwrap()
            .rows
            .into_iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(11), Value::Int(12), Value::Int(21)]
    );

    root.sql("INSERT INTO progress_aggregate SELECT id, insert_select_visible_count('progress_aggregate') FROM progress_source GROUP BY id", &[]).unwrap();
    assert_eq!(
        sorted_seen(&root, "progress_aggregate"),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );

    root.sql("INSERT INTO progress_window SELECT id, insert_select_visible_count('progress_window') + row_number() OVER (ORDER BY id) * 0 FROM progress_source", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_window"),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );

    root.sql("INSERT INTO progress_union_all SELECT id, insert_select_visible_count('progress_union_all') FROM progress_source WHERE id <= 2 UNION ALL SELECT id, insert_select_visible_count('progress_union_all') FROM progress_source WHERE id > 2", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_union_all"),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );

    root.sql("INSERT INTO progress_union_distinct SELECT id, insert_select_visible_count('progress_union_distinct') FROM progress_source WHERE id <= 2 UNION SELECT id, insert_select_visible_count('progress_union_distinct') FROM progress_source WHERE id > 2", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_union_distinct"),
        vec![Value::Int(0), Value::Int(0), Value::Int(0)]
    );

    root.sql("INSERT INTO progress_union_limit SELECT id, insert_select_visible_count('progress_union_limit') FROM progress_source WHERE id <= 2 UNION ALL SELECT id, insert_select_visible_count('progress_union_limit') FROM progress_source WHERE id > 2 LIMIT 2", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_union_limit"),
        vec![Value::Int(0), Value::Int(1)]
    );

    root.sql("INSERT INTO progress_union_offset SELECT id, insert_select_visible_count('progress_union_offset') FROM progress_source WHERE id <= 2 UNION ALL SELECT id, insert_select_visible_count('progress_union_offset') FROM progress_source WHERE id > 2 LIMIT 1 OFFSET 1", &[]).unwrap();
    assert_eq!(seen(&root, "progress_union_offset"), vec![Value::Int(0)]);
    assert_eq!(
        root.sql("SELECT id FROM progress_union_offset", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(2)
    );
}

#[test]
fn ordered_aggregate_and_window_defer_volatile_output_until_after_limit() {
    let directory = tempfile::tempdir().unwrap();
    let root = Arc::new(Engine::open(&directory.path().join("ordered-progress.db")).unwrap());
    register_visible_count(&root);
    root.sql("CREATE TABLE progress_source (id INTEGER PRIMARY KEY); INSERT INTO progress_source VALUES (1), (2), (3); CREATE TABLE progress_aggregate_order (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_window_order (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_aggregate_target_order (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_window_target_order (id INTEGER PRIMARY KEY, seen INTEGER)", &[]).unwrap();

    root.sql("INSERT INTO progress_aggregate_order SELECT id, insert_select_visible_count('progress_aggregate_order') FROM progress_source GROUP BY id ORDER BY id LIMIT 2", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_aggregate_order"),
        vec![Value::Int(0), Value::Int(1)]
    );

    root.sql("INSERT INTO progress_window_order SELECT id, insert_select_visible_count('progress_window_order') + row_number() OVER (ORDER BY id) * 0 FROM progress_source ORDER BY id LIMIT 2", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_window_order"),
        vec![Value::Int(0), Value::Int(1)]
    );

    root.sql("INSERT INTO progress_aggregate_target_order SELECT id, insert_select_visible_count('progress_aggregate_target_order') AS seen FROM progress_source GROUP BY id ORDER BY insert_select_visible_count('progress_aggregate_target_order'), id LIMIT 2", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_aggregate_target_order"),
        vec![Value::Int(0), Value::Int(0)]
    );

    root.sql("INSERT INTO progress_window_target_order SELECT id, insert_select_visible_count('progress_window_target_order') + row_number() OVER (ORDER BY id) * 0 AS seen FROM progress_source ORDER BY seen, id LIMIT 2", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_window_target_order"),
        vec![Value::Int(0), Value::Int(0)]
    );
}

#[test]
fn update_and_delete_qualify_against_preceding_command_rows() {
    let directory = tempfile::tempdir().unwrap();
    let root = Arc::new(Engine::open(&directory.path().join("update-delete-progress.db")).unwrap());
    register_visible_count(&root);
    register_visible_sum(&root);
    root.sql("CREATE TABLE progress_update_where (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_delete_where (id INTEGER PRIMARY KEY); INSERT INTO progress_update_where VALUES (1, 10), (2, 20), (3, 30); INSERT INTO progress_delete_where VALUES (1), (2), (3)", &[]).unwrap();

    root.sql("UPDATE progress_update_where SET seen = command_progress_visible_sum('progress_update_where') WHERE command_progress_visible_sum('progress_update_where') = CASE id WHEN 1 THEN 60 WHEN 2 THEN 110 ELSE 200 END", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_update_where"),
        vec![Value::Int(60), Value::Int(110), Value::Int(200)]
    );

    root.sql("DELETE FROM progress_delete_where WHERE insert_select_visible_count('progress_delete_where') = 4 - id", &[]).unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM progress_delete_where", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(0)
    );
}

#[test]
fn merge_actions_see_preceding_command_rows() {
    let directory = tempfile::tempdir().unwrap();
    let root = Arc::new(Engine::open(&directory.path().join("merge-progress.db")).unwrap());
    register_visible_count(&root);
    register_visible_sum(&root);
    root.sql("CREATE TABLE progress_merge_source (id INTEGER PRIMARY KEY); INSERT INTO progress_merge_source VALUES (1), (2), (3); CREATE TABLE progress_merge_update (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_merge_delete (id INTEGER PRIMARY KEY); CREATE TABLE progress_merge_insert (id INTEGER PRIMARY KEY, seen INTEGER); INSERT INTO progress_merge_update VALUES (1, 10), (2, 20), (3, 30); INSERT INTO progress_merge_delete VALUES (1), (2), (3)", &[]).unwrap();

    root.sql("MERGE INTO progress_merge_update AS target USING progress_merge_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET seen = command_progress_visible_sum('progress_merge_update')", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_merge_update"),
        vec![Value::Int(60), Value::Int(110), Value::Int(200)]
    );

    root.sql("MERGE INTO progress_merge_delete AS target USING progress_merge_source AS source ON target.id = source.id WHEN MATCHED AND insert_select_visible_count('progress_merge_delete') = 4 - source.id THEN DELETE", &[]).unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM progress_merge_delete", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(0)
    );

    root.sql("MERGE INTO progress_merge_insert AS target USING progress_merge_source AS source ON target.id = source.id WHEN NOT MATCHED THEN INSERT (id, seen) VALUES (source.id, insert_select_visible_count('progress_merge_insert'))", &[]).unwrap();
    assert_eq!(
        seen(&root, "progress_merge_insert"),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );
}
