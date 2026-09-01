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
        Value::Str("first=11/t/1;last=14/t/1;prior=13/t;absolute=12/t;relative=11/t;".into())
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
        Value::Str("forward=2/t;back_all=1/t;next=1/t;missing=0/f;exhausted=/f/0".into())
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

#[test]
fn row_returning_commands_execute_only_when_the_portal_runs() {
    let engine = engine();
    exec(
        &engine,
        "CREATE TABLE plpgsql_command_source(id integer PRIMARY KEY, value integer)",
    );
    exec(
        &engine,
        "INSERT INTO plpgsql_command_source VALUES (1, 10), (2, 20)",
    );
    exec(
        &engine,
        "CREATE FUNCTION command_cursor_lifecycle(fetch_it boolean) RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c refcursor; fetched integer;
         BEGIN
           OPEN c FOR UPDATE plpgsql_command_source AS t
             SET value = t.value + 1 RETURNING t.value;
           IF fetch_it THEN
             FETCH c INTO fetched;
           END IF;
           CLOSE c;
           RETURN coalesce(fetched::text, 'closed');
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT command_cursor_lifecycle(false) AS value"),
        Value::Str("closed".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT sum(value) FROM plpgsql_command_source"),
        Value::Int(30)
    );
    assert_eq!(
        scalar(&engine, "SELECT command_cursor_lifecycle(true) AS value"),
        Value::Str("11".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT sum(value) FROM plpgsql_command_source"),
        Value::Int(32)
    );

    exec(
        &engine,
        "CREATE FUNCTION command_cursor_failure(fetch_it boolean) RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c refcursor; fetched integer;
         BEGIN
           OPEN c FOR UPDATE plpgsql_command_source AS t
             SET value = 1 / (t.id - 2) RETURNING t.value;
           IF fetch_it THEN FETCH c INTO fetched; END IF;
           CLOSE c;
           RETURN 'ok';
         EXCEPTION WHEN OTHERS THEN
           RETURN SQLSTATE || '|' || SQLERRM;
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT command_cursor_failure(false) AS value"),
        Value::Str("ok".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT command_cursor_failure(true) AS value"),
        Value::Str("22012|division by zero".into())
    );
}

#[test]
fn returned_command_cursor_uses_the_first_fetch_statement_state() {
    let engine = engine();
    exec(
        &engine,
        "CREATE TABLE plpgsql_returned_command(id integer PRIMARY KEY, value integer)",
    );
    exec(
        &engine,
        "INSERT INTO plpgsql_returned_command VALUES (1, 10), (2, 20)",
    );
    exec(
        &engine,
        "CREATE FUNCTION return_command_cursor() RETURNS refcursor LANGUAGE plpgsql AS $$
         DECLARE c refcursor := 'returned command';
         BEGIN
           OPEN c FOR UPDATE plpgsql_returned_command AS t
             SET value = t.value + 5 RETURNING t.id, t.value;
           RETURN c;
         END
         $$",
    );
    exec(&engine, "BEGIN");
    assert_eq!(
        scalar(&engine, "SELECT return_command_cursor() AS value"),
        Value::Str("returned command".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT sum(value) FROM plpgsql_returned_command"),
        Value::Int(30)
    );
    exec(
        &engine,
        "UPDATE plpgsql_returned_command SET value = 100 WHERE id = 2",
    );
    assert_eq!(
        scalar(&engine, "FETCH NEXT FROM \"returned command\""),
        Value::Int(1)
    );
    assert_eq!(
        scalar(&engine, "SELECT sum(value) FROM plpgsql_returned_command"),
        Value::Int(120)
    );
    exec(&engine, "COMMIT");
}

#[test]
fn dynamic_command_cursor_shapes_match_postgresql() {
    let engine = engine();
    exec(
        &engine,
        "CREATE TABLE plpgsql_dynamic_command(id integer PRIMARY KEY, value integer)",
    );
    exec(
        &engine,
        "INSERT INTO plpgsql_dynamic_command VALUES (1, 10), (2, 20)",
    );
    exec(
        &engine,
        "CREATE FUNCTION run_command_cursor(query_text text) RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c refcursor; fetched text;
         BEGIN
           OPEN c FOR EXECUTE query_text;
           FETCH c INTO fetched;
           CLOSE c;
           RETURN coalesce(fetched, '<null>');
         EXCEPTION WHEN OTHERS THEN
           RETURN SQLSTATE || '|' || SQLERRM;
         END
         $$",
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT run_command_cursor('INSERT INTO plpgsql_dynamic_command VALUES (3, 30)') AS value"
        ),
        Value::Str("42P11|cannot open INSERT query as cursor".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM plpgsql_dynamic_command"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT run_command_cursor('UPDATE plpgsql_dynamic_command SET value = value + 1 WHERE id = 1 RETURNING value') AS value"
        ),
        Value::Str("11".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT run_command_cursor('DELETE FROM plpgsql_dynamic_command WHERE id = 2 RETURNING id') AS value"
        ),
        Value::Str("2".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT run_command_cursor('SHOW search_path') AS value"
        ),
        Value::Str("public".into())
    );
    let explain = scalar(
        &engine,
        "SELECT run_command_cursor('EXPLAIN SELECT * FROM plpgsql_dynamic_command') AS value",
    );
    assert!(
        matches!(explain, Value::Str(ref value) if !value.starts_with("42P11|")),
        "unexpected EXPLAIN cursor result: {explain:?}"
    );
}

#[test]
fn command_cursor_zero_move_and_scrolling_match_postgresql() {
    let engine = engine();
    exec(
        &engine,
        "CREATE TABLE plpgsql_dynamic_command(id integer PRIMARY KEY, value integer)",
    );
    exec(
        &engine,
        "INSERT INTO plpgsql_dynamic_command VALUES (1, 10), (2, 20)",
    );
    exec(
        &engine,
        "CREATE FUNCTION command_cursor_controls(mode integer) RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c refcursor; fetched integer; moved bigint; report text; observed integer;
         BEGIN
           IF mode = 0 THEN
             OPEN c FOR EXECUTE
               'UPDATE plpgsql_dynamic_command SET value = value + 1 RETURNING value';
             MOVE FORWARD 0 FROM c;
             GET DIAGNOSTICS moved = ROW_COUNT;
             report := format('%s/%s;', moved, FOUND);
             SELECT min(value) INTO observed FROM plpgsql_dynamic_command;
             CLOSE c;
             RETURN report || observed::text;
           ELSIF mode = 1 THEN
             OPEN c FOR EXECUTE
               'UPDATE plpgsql_dynamic_command SET value = value + 1 RETURNING value';
             FETCH c INTO fetched;
             BEGIN FETCH PRIOR FROM c INTO fetched;
             EXCEPTION WHEN OTHERS THEN CLOSE c; RETURN SQLSTATE || '|' || SQLERRM;
             END;
           ELSE
             OPEN c SCROLL FOR EXECUTE
               'UPDATE plpgsql_dynamic_command SET value = value + 1 RETURNING value';
             FETCH c INTO fetched;
             report := format('%s/%s', fetched, FOUND);
             CLOSE c;
             RETURN report;
           END IF;
           CLOSE c;
           RETURN 'no error';
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT command_cursor_controls(0) AS value"),
        Value::Str("0/f;11".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT command_cursor_controls(1) AS value"),
        Value::Str("55000|cursor can only scan forward".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT command_cursor_controls(2) AS value"),
        Value::Str("/t".into())
    );
}

#[test]
fn call_output_cursor_is_deferred_and_obeys_command_scrolling() {
    let engine = engine();
    exec(&engine, "CREATE TABLE cursor_call_log(value integer)");
    exec(
        &engine,
        "CREATE PROCEDURE cursor_call_out(IN input integer, OUT output integer) LANGUAGE plpgsql AS $$
         BEGIN INSERT INTO cursor_call_log VALUES (input); output := input + 1; END
         $$",
    );
    exec(
        &engine,
        "CREATE PROCEDURE cursor_call_no_out(IN input integer) LANGUAGE plpgsql AS $$
         BEGIN INSERT INTO cursor_call_log VALUES (input); END
         $$",
    );
    exec(
        &engine,
        "CREATE FUNCTION run_call_cursor(query_text text, fetch_it boolean, scroll_it boolean)
         RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c refcursor; fetched text; report text;
         BEGIN
           IF scroll_it THEN OPEN c SCROLL FOR EXECUTE query_text;
           ELSE OPEN c FOR EXECUTE query_text;
           END IF;
           IF fetch_it THEN
             FETCH c INTO fetched;
             report := format('fetch=%s/%s;', fetched, FOUND);
             BEGIN FETCH PRIOR FROM c INTO fetched;
             EXCEPTION WHEN OTHERS THEN CLOSE c; RETURN report || SQLSTATE || '|' || SQLERRM;
             END;
             report := report || format('prior=%s/%s', fetched, FOUND);
           ELSE report := 'open';
           END IF;
           CLOSE c; RETURN report;
         EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE || '|' || SQLERRM;
         END
         $$",
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT run_call_cursor('CALL cursor_call_out(4, NULL)', false, false)"
        ),
        Value::Str("open".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM cursor_call_log"),
        Value::Int(0)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT run_call_cursor('CALL cursor_call_out(4, NULL)', true, false)"
        ),
        Value::Str("fetch=5/t;55000|cursor can only scan forward".into())
    );
    exec(&engine, "TRUNCATE cursor_call_log");
    assert_eq!(
        scalar(
            &engine,
            "SELECT run_call_cursor('CALL cursor_call_out(5, NULL)', true, true)"
        ),
        Value::Str("fetch=6/t;prior=/f".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT run_call_cursor('CALL cursor_call_no_out(6)', false, false)"
        ),
        Value::Str("42P11|cannot open CALL query as cursor".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT sum(value) FROM cursor_call_log"),
        Value::Int(5)
    );
    exec(
        &engine,
        "CREATE FUNCTION run_static_call_cursor() RETURNS integer LANGUAGE plpgsql AS $$
         DECLARE c refcursor; fetched integer;
         BEGIN
           OPEN c FOR CALL cursor_call_out(7, NULL);
           FETCH c INTO fetched;
           CLOSE c;
           RETURN fetched;
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT run_static_call_cursor()"),
        Value::Int(8)
    );
    assert_eq!(
        scalar(&engine, "SELECT sum(value) FROM cursor_call_log"),
        Value::Int(12)
    );
}

#[test]
fn merge_returning_cursor_is_deferred_and_rejects_scrolling() {
    let engine = engine();
    exec(
        &engine,
        "CREATE TABLE cursor_merge_target(id integer PRIMARY KEY, value integer NOT NULL)",
    );
    exec(&engine, "INSERT INTO cursor_merge_target VALUES (1, 10)");
    exec(
        &engine,
        "CREATE FUNCTION run_merge_cursor(fetch_it boolean, scroll_it boolean, returning_it boolean)
         RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c refcursor; fetched_id integer; fetched_value integer;
         BEGIN
           IF returning_it THEN
             IF scroll_it THEN
               OPEN c SCROLL FOR EXECUTE
                 'MERGE INTO cursor_merge_target AS target USING (VALUES (1, 5), (2, 7)) AS source(id, delta) ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = target.value + source.delta WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.delta) RETURNING target.id, target.value';
             ELSE
               OPEN c FOR EXECUTE
                 'MERGE INTO cursor_merge_target AS target USING (VALUES (1, 5), (2, 7)) AS source(id, delta) ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = target.value + source.delta WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.delta) RETURNING target.id, target.value';
             END IF;
           ELSE
             OPEN c FOR EXECUTE
               'MERGE INTO cursor_merge_target AS target USING (VALUES (1, 5)) AS source(id, delta) ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = target.value + source.delta';
           END IF;
           IF fetch_it THEN
             FETCH c INTO fetched_id, fetched_value;
             CLOSE c;
             RETURN format('%s/%s/%s', fetched_id, fetched_value, FOUND);
           END IF;
           CLOSE c;
           RETURN 'open';
         EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE || '|' || SQLERRM;
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT run_merge_cursor(false, false, true)"),
        Value::Str("open".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT sum(value) FROM cursor_merge_target"),
        Value::Int(10)
    );
    assert_eq!(
        scalar(&engine, "SELECT run_merge_cursor(true, false, true)"),
        Value::Str("1/15/t".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT sum(value) FROM cursor_merge_target"),
        Value::Int(22)
    );
    exec(&engine, "TRUNCATE cursor_merge_target");
    exec(&engine, "INSERT INTO cursor_merge_target VALUES (1, 10)");
    assert_eq!(
        scalar(&engine, "SELECT run_merge_cursor(true, true, true)"),
        Value::Str("0A000|DECLARE SCROLL CURSOR ... FOR UPDATE/SHARE is not supported".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT run_merge_cursor(false, false, false)"),
        Value::Str("42P11|cannot open MERGE query as cursor".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT sum(value) FROM cursor_merge_target"),
        Value::Int(10)
    );
    exec(
        &engine,
        "CREATE FUNCTION run_static_merge_cursor() RETURNS integer LANGUAGE plpgsql AS $$
         DECLARE c refcursor; fetched integer;
         BEGIN
           OPEN c FOR MERGE INTO cursor_merge_target AS target
             USING (VALUES (1, 2)) AS source(id, delta) ON target.id = source.id
             WHEN MATCHED THEN UPDATE SET value = target.value + source.delta
             RETURNING target.value;
           FETCH c INTO fetched;
           CLOSE c;
           RETURN fetched;
         END
         $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT run_static_merge_cursor()"),
        Value::Int(12)
    );
}

#[test]
fn bound_cursor_for_uses_named_arguments_loop_scope_and_single_row_fetches() {
    let engine = engine();
    exec(&engine, "CREATE TABLE cursor_for_values(v integer)");
    exec(
        &engine,
        "INSERT INTO cursor_for_values VALUES (1),(2),(3),(4)",
    );
    exec(
        &engine,
        "CREATE FUNCTION bound_cursor_for_report(low integer, high integer)
         RETURNS text LANGUAGE plpgsql AS $$
         DECLARE loop_row text := 'outer';
                 c CURSOR (low_value integer, high_value integer) FOR
                   SELECT v, v * 10 AS ten FROM cursor_for_values
                   WHERE v BETWEEN low_value AND high_value ORDER BY v;
                 output text := ''; loop_found boolean;
         BEGIN
           PERFORM 1 WHERE false;
           <<walk>>
           FOR loop_row IN c(high_value => high, low_value => low) LOOP
             output := output || loop_row.v || ':' || loop_row.ten || ':' || FOUND || ',';
             EXIT walk WHEN loop_row.v = 2;
           END LOOP walk;
           loop_found := FOUND;
           RETURN output || 'loop=' || loop_found || '|outer=' || loop_row || '|null=' || (c IS NULL);
         END $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT bound_cursor_for_report(1, 3)"),
        Value::Str("1:10:true,2:20:true,loop=true|outer=outer|null=true".into())
    );

    exec(
        &engine,
        "CREATE FUNCTION bound_cursor_for_found_timing() RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c CURSOR FOR SELECT v FROM cursor_for_values WHERE v <= 2 ORDER BY v;
                 output text := '';
         BEGIN
           PERFORM 1 WHERE false;
           FOR loop_row IN c LOOP output := output || FOUND || ','; END LOOP;
           RETURN output || 'after=' || FOUND || '|null=' || (c IS NULL);
         END $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT bound_cursor_for_found_timing()"),
        Value::Str("false,false,after=true|null=true".into())
    );

    exec(&engine, "CREATE SEQUENCE cursor_for_sequence START WITH 1");
    exec(
        &engine,
        "CREATE FUNCTION bound_cursor_for_prefetch() RETURNS bigint LANGUAGE plpgsql AS $$
         DECLARE c CURSOR FOR
                   SELECT nextval('cursor_for_sequence') AS value FROM generate_series(1, 100);
         BEGIN
           FOR loop_row IN c LOOP EXIT; END LOOP;
           RETURN currval('cursor_for_sequence');
         END $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT bound_cursor_for_prefetch()"),
        Value::Int(1)
    );
}

#[test]
fn bound_cursor_for_closes_pinned_portals_and_restores_explicit_names() {
    let engine = engine();
    exec(&engine, "CREATE TABLE cursor_for_cleanup(v integer)");
    exec(&engine, "INSERT INTO cursor_for_cleanup VALUES (7),(8)");
    exec(
        &engine,
        "CREATE FUNCTION bound_cursor_for_custom_name() RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c CURSOR FOR SELECT v FROM cursor_for_cleanup ORDER BY v; output text := '';
         BEGIN
           c := 'cursor_for_custom';
           FOR loop_row IN c LOOP output := output || loop_row.v || ','; EXIT; END LOOP;
           RETURN output || c::text || '|' || FOUND;
         END $$",
    );
    for _ in 0..2 {
        assert_eq!(
            scalar(&engine, "SELECT bound_cursor_for_custom_name()"),
            Value::Str("7,cursor_for_custom|true".into())
        );
    }

    exec(
        &engine,
        "CREATE FUNCTION bound_cursor_for_reassign_name() RETURNS text LANGUAGE plpgsql AS $$
         DECLARE c CURSOR FOR SELECT v FROM cursor_for_cleanup ORDER BY v;
         BEGIN
           c := 'cursor_for_initial';
           FOR loop_row IN c LOOP c := 'cursor_for_changed'; EXIT; END LOOP;
           RETURN c::text;
         END $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT bound_cursor_for_reassign_name()"),
        Value::Str("cursor_for_changed".into())
    );

    exec(
        &engine,
        "CREATE FUNCTION bound_cursor_for_return() RETURNS integer LANGUAGE plpgsql AS $$
         DECLARE c CURSOR FOR SELECT v FROM cursor_for_cleanup ORDER BY v;
         BEGIN
           c := 'cursor_for_return';
           FOR loop_row IN c LOOP RETURN loop_row.v; END LOOP;
           RETURN 0;
         END $$",
    );
    assert_eq!(
        scalar(&engine, "SELECT bound_cursor_for_return()"),
        Value::Int(7)
    );
    assert_eq!(
        scalar(&engine, "SELECT bound_cursor_for_return()"),
        Value::Int(7)
    );

    exec(
        &engine,
        "CREATE FUNCTION bound_cursor_for_close_inside() RETURNS integer LANGUAGE plpgsql AS $$
         DECLARE c CURSOR FOR SELECT v FROM cursor_for_cleanup ORDER BY v;
         BEGIN
           c := 'cursor_for_pinned';
           FOR loop_row IN c LOOP CLOSE c; RETURN loop_row.v; END LOOP;
           RETURN 0;
         END $$",
    );
    let err = exec_err(&engine, "SELECT bound_cursor_for_close_inside()");
    assert_eq!(err.sqlstate(), Some("24000"));
    assert!(
        err.to_string()
            .contains("cannot drop pinned portal \"cursor_for_pinned\""),
        "got: {err}"
    );
    assert_eq!(
        scalar(&engine, "SELECT bound_cursor_for_custom_name()"),
        Value::Str("7,cursor_for_custom|true".into())
    );

    exec(
        &engine,
        "CREATE FUNCTION bound_cursor_for_already_open() RETURNS integer LANGUAGE plpgsql AS $$
         DECLARE c CURSOR FOR SELECT v FROM cursor_for_cleanup;
         BEGIN
           c := 'cursor_for_busy';
           OPEN c;
           FOR loop_row IN c LOOP RETURN loop_row.v; END LOOP;
           RETURN 0;
         END $$",
    );
    let err = exec_err(&engine, "SELECT bound_cursor_for_already_open()");
    assert_eq!(err.sqlstate(), Some("42P03"));
    assert!(
        err.to_string()
            .contains("cursor \"cursor_for_busy\" already in use"),
        "got: {err}"
    );
}
