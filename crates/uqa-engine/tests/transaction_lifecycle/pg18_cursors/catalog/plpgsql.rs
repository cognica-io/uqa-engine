//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn pg18_cursor_catalog_preserves_static_bound_and_dynamic_query_source() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE cursor_observed(name text, source text, scrollable boolean)",
            &[],
        )
        .unwrap();
    engine.sql(
        "DO $body$ DECLARE c refcursor := 'static'; d refcursor := 'dynamic'; utility refcursor := 'command'; b CURSOR (n integer) FOR SELECT n; BEGIN
           OPEN c FOR SELECT 42;
           b := 'bound'; OPEN b(7);
           OPEN d SCROLL FOR EXECUTE 'SELECT  $1 /* original source */' USING 9;
           OPEN utility FOR SHOW work_mem;
           INSERT INTO cursor_observed SELECT name, statement, is_scrollable FROM pg_cursors;
           CLOSE c; CLOSE b; CLOSE d; CLOSE utility;
         END $body$", &[]).unwrap();
    let result = engine
        .sql(
            "SELECT name, source, scrollable FROM cursor_observed ORDER BY name",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 4);
    for (index, (name, source, scrollable)) in [
        ("bound", "SELECT n", false),
        ("command", "SHOW work_mem", false),
        ("dynamic", "SELECT  $1 /* original source */", true),
        ("static", "SELECT 42", false),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(result.value_at(index, 0), Some(&Value::Str(name.into())));
        assert_eq!(result.value_at(index, 1), Some(&Value::Str(source.into())));
        assert_eq!(result.value_at(index, 2), Some(&Value::Bool(scrollable)));
    }
    assert!(names(&engine).is_empty());
}

#[test]
fn pg18_cursor_catalog_keeps_loop_declaration_flags_after_procedural_commit() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE cursor_loop_observed(source text, held boolean, scrollable boolean)",
            &[],
        )
        .unwrap();
    engine.sql(
        "DO $body$ DECLARE r record; BEGIN
           FOR r IN SELECT * FROM generate_series(1, 2) LOOP
             COMMIT;
             INSERT INTO cursor_loop_observed SELECT statement, is_holdable, is_scrollable FROM pg_cursors;
             EXIT;
           END LOOP;
           FOR r IN EXECUTE 'SELECT  3 /* dynamic loop */' LOOP
             INSERT INTO cursor_loop_observed SELECT statement, is_holdable, is_scrollable FROM pg_cursors;
           END LOOP;
         END $body$", &[]).unwrap();
    let result = engine
        .sql(
            "SELECT source, held, scrollable FROM cursor_loop_observed ORDER BY source",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    for (index, source) in [
        "SELECT  3 /* dynamic loop */",
        "SELECT * FROM generate_series(1, 2)",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(result.value_at(index, 0), Some(&Value::Str(source.into())));
        assert_eq!(result.value_at(index, 1), Some(&Value::Bool(false)));
        assert_eq!(result.value_at(index, 2), Some(&Value::Bool(false)));
    }
    assert!(names(&engine).is_empty());
}

#[test]
fn pg18_unnamed_cursor_skips_explicit_names_and_exception_cleanup_releases_metadata() {
    let engine = Engine::new();
    engine
        .sql(
            "BEGIN; DECLARE \"<unnamed portal 1>\" CURSOR FOR SELECT 10",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "DO $body$ DECLARE c refcursor; BEGIN OPEN c FOR SELECT 20; END $body$",
            &[],
        )
        .unwrap();
    assert_eq!(names(&engine), ["<unnamed portal 1>", "<unnamed portal 2>"]);
    engine.sql("DO $body$ DECLARE c refcursor := 'failed'; BEGIN BEGIN OPEN c FOR SELECT 30; RAISE EXCEPTION 'abort scope'; EXCEPTION WHEN OTHERS THEN NULL; END; END $body$", &[]).unwrap();
    assert_eq!(names(&engine), ["<unnamed portal 1>", "<unnamed portal 2>"]);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert!(names(&engine).is_empty());
}
