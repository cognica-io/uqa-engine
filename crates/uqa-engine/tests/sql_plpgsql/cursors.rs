//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 PL/pgSQL cursor opening, scrolling, fetching, and movement.

use super::*;

fn install_cursor_source(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE plpgsql_cursor_source(id integer PRIMARY KEY, value text)",
    );
    exec(
        engine,
        "INSERT INTO plpgsql_cursor_source VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'd')",
    );
}

#[test]
fn static_open_captures_variables_and_supports_every_single_row_fetch_direction() {
    let engine = engine();
    install_cursor_source(&engine);
    exec(
        &engine,
        "CREATE FUNCTION cursor_static(seed integer) RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c refcursor; value integer := -99; moved integer; report text := '';
         BEGIN
           OPEN c SCROLL FOR SELECT id + seed FROM plpgsql_cursor_source ORDER BY id;
           seed := 1000;
           FETCH FIRST FROM c INTO value;
           GET DIAGNOSTICS moved = ROW_COUNT;
           report := report || format('first=%s/%s/%s;', value, FOUND, moved);
           FETCH LAST FROM c INTO value;
           GET DIAGNOSTICS moved = ROW_COUNT;
           report := report || format('last=%s/%s/%s;', value, FOUND, moved);
           FETCH PRIOR FROM c INTO value;
           report := report || format('prior=%s/%s;', value, FOUND);
           FETCH ABSOLUTE (seed / 500) FROM c INTO value;
           report := report || format('absolute=%s/%s;', value, FOUND);
           FETCH RELATIVE (-1) FROM c INTO value;
           report := report || format('relative=%s/%s;', value, FOUND);
           CLOSE c;
           RETURN report;
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT cursor_static(10) AS value"),
        Value::Str(
            "first=11/true/1;last=14/true/1;prior=13/true;absolute=12/true;relative=11/true;"
                .into()
        )
    );
}

#[test]
fn move_supports_direction_expressions_all_found_and_row_count() {
    let engine = engine();
    install_cursor_source(&engine);
    exec(
        &engine,
        "CREATE FUNCTION cursor_move() RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c SCROLL CURSOR FOR SELECT id FROM plpgsql_cursor_source ORDER BY id;
                 value integer := -99; moved integer; report text := '';
         BEGIN
           OPEN c;
           MOVE FORWARD 2 FROM c;
           GET DIAGNOSTICS moved = ROW_COUNT;
           report := report || format('forward=%s/%s;', moved, FOUND);
           MOVE BACKWARD ALL FROM c;
           GET DIAGNOSTICS moved = ROW_COUNT;
           report := report || format('back_all=%s/%s;', moved, FOUND);
           FETCH NEXT FROM c INTO value;
           report := report || format('next=%s/%s;', value, FOUND);
           MOVE ABSOLUTE 99 FROM c;
           GET DIAGNOSTICS moved = ROW_COUNT;
           report := report || format('missing=%s/%s;', moved, FOUND);
           FETCH NEXT FROM c INTO value;
           GET DIAGNOSTICS moved = ROW_COUNT;
           report := report || format('exhausted=%s/%s/%s', value, FOUND, moved);
           CLOSE c;
           RETURN report;
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT cursor_move() AS value"),
        Value::Str(
            "forward=2/true;back_all=1/true;next=1/true;missing=0/false;exhausted=/false/0".into()
        )
    );
}

#[test]
fn dynamic_open_uses_parameters_and_returns_a_transaction_portal() {
    let engine = engine();
    install_cursor_source(&engine);
    exec(
        &engine,
        "CREATE FUNCTION cursor_dynamic(seed integer) RETURNS refcursor LANGUAGE plpgsql AS $$
         DECLARE c refcursor := 'dynamic portal';
         BEGIN
           OPEN c SCROLL FOR EXECUTE
             'SELECT id::text || $1 AS value FROM plpgsql_cursor_source ORDER BY id'
             USING seed::text;
           MOVE FORWARD seed / seed + 1 FROM c;
           RETURN c;
         END
         $$",
    );
    exec(&engine, "BEGIN");
    assert_eq!(
        scalar(&engine, "SELECT cursor_dynamic(7) AS value"),
        Value::Str("dynamic portal".into())
    );
    assert_eq!(
        scalar(&engine, "FETCH BACKWARD FROM \"dynamic portal\""),
        Value::Str("17".into())
    );
    assert_eq!(
        scalar(&engine, "FETCH RELATIVE 1 FROM \"dynamic portal\""),
        Value::Str("27".into())
    );
    exec(&engine, "COMMIT");
}

#[test]
fn cursor_scroll_and_null_diagnostics_match_postgresql() {
    let engine = engine();
    install_cursor_source(&engine);
    exec(
        &engine,
        "CREATE FUNCTION cursor_no_scroll() RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c NO SCROLL CURSOR FOR SELECT id FROM plpgsql_cursor_source ORDER BY id;
                 value integer;
         BEGIN
           OPEN c;
           FETCH NEXT FROM c INTO value;
           BEGIN FETCH PRIOR FROM c INTO value;
           EXCEPTION WHEN OTHERS THEN CLOSE c; RETURN SQLSTATE || '|' || SQLERRM;
           END;
           CLOSE c; RETURN 'no error';
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT cursor_no_scroll() AS value"),
        Value::Str("55000|cursor can only scan forward".into())
    );
    exec(
        &engine,
        "CREATE FUNCTION cursor_nulls(selector integer) RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c refcursor; value integer;
         BEGIN
           IF selector IS NULL THEN
             BEGIN FETCH c INTO value;
             EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE || '|' || SQLERRM;
             END;
           END IF;
           OPEN c FOR SELECT id FROM plpgsql_cursor_source;
           BEGIN FETCH ABSOLUTE NULL::integer FROM c INTO value;
           EXCEPTION WHEN OTHERS THEN CLOSE c; RETURN SQLSTATE || '|' || SQLERRM;
           END;
           CLOSE c; RETURN 'no error';
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT cursor_nulls(NULL) AS value"),
        Value::Str("22004|cursor variable \"c\" is null".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT cursor_nulls(1) AS value"),
        Value::Str("22004|relative or absolute cursor position is null".into())
    );
}

#[test]
fn duplicate_and_scroll_lock_open_errors_match_postgresql() {
    let engine = engine();
    install_cursor_source(&engine);
    exec(
        &engine,
        "CREATE FUNCTION cursor_open_errors(locking boolean) RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c1 refcursor := 'duplicate portal'; c2 refcursor := 'duplicate portal';
         BEGIN
           IF locking THEN
             BEGIN OPEN c1 SCROLL FOR SELECT id FROM plpgsql_cursor_source FOR UPDATE;
             EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE || '|' || SQLERRM;
             END;
           END IF;
           OPEN c1 FOR SELECT 1;
           BEGIN OPEN c2 FOR EXECUTE NULL::text;
           EXCEPTION WHEN OTHERS THEN CLOSE c1; RETURN SQLSTATE || '|' || SQLERRM;
           END;
           CLOSE c1; RETURN 'no error';
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT cursor_open_errors(false) AS value"),
        Value::Str("42P03|cursor \"duplicate portal\" already in use".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT cursor_open_errors(true) AS value"),
        Value::Str("0A000|DECLARE SCROLL CURSOR ... FOR UPDATE/SHARE is not supported".into())
    );
}
