//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Names that are both a `PL/pgSQL` variable and a column of a queried relation, resolved as `PostgreSQL` 18 resolves them under `plpgsql.variable_conflict` and `#variable_conflict`.

use super::*;

const AMBIGUOUS_DETAIL: &str = "It could refer to either a PL/pgSQL variable or a table column.";

fn tables() -> Engine {
    let engine = engine();
    exec(&engine, "CREATE TABLE t (id int, v int)");
    exec(&engine, "INSERT INTO t VALUES (10, 1), (20, 2)");
    exec(&engine, "CREATE TABLE tv (id int PRIMARY KEY, v int)");
    engine
}

fn ambiguous(engine: &Engine, call: &str, name: &str) {
    let error = exec_err(engine, call);
    let detail = match &error {
        SQLError::Diagnostic { detail, .. } => detail.clone(),
        _ => None,
    };
    assert_eq!(
        (error.sqlstate(), error.to_string(), detail.as_deref()),
        (
            Some("42702"),
            format!("column reference \"{name}\" is ambiguous"),
            Some(AMBIGUOUS_DETAIL)
        ),
        "{call}"
    );
}

#[test]
fn a_name_of_a_variable_and_a_column_is_ambiguous_when_its_statement_runs() {
    let engine = tables();
    for (definition, call, name) in [
        (
            "CREATE FUNCTION p_sel() RETURNS int LANGUAGE plpgsql AS $$ DECLARE id int := 5; r int; BEGIN SELECT id INTO r FROM t ORDER BY v LIMIT 1; RETURN r; END $$",
            "SELECT p_sel()",
            "id",
        ),
        (
            "CREATE FUNCTION p_wr() RETURNS text LANGUAGE plpgsql AS $$ DECLARE t int := 5; r text; BEGIN SELECT t::text INTO r FROM t ORDER BY id LIMIT 1; RETURN r; END $$",
            "SELECT p_wr()",
            "t",
        ),
        (
            "CREATE FUNCTION p_rf() RETURNS int LANGUAGE plpgsql AS $$ DECLARE t record; r int; BEGIN SELECT 7 AS id INTO t; SELECT t.id INTO r FROM t ORDER BY id LIMIT 1; RETURN r; END $$",
            "SELECT p_rf()",
            "t.id",
        ),
        (
            "CREATE FUNCTION p_us() RETURNS int LANGUAGE plpgsql AS $$ DECLARE v int := 100; BEGIN UPDATE tv SET v = v WHERE tv.id = 42; RETURN 0; END $$",
            "SELECT p_us()",
            "v",
        ),
        (
            "CREATE FUNCTION p_gc() RETURNS bigint LANGUAGE plpgsql AS $$ DECLARE v int := 5; r bigint; BEGIN SELECT count(*) INTO r FROM (SELECT id FROM t GROUP BY v) s; RETURN r; END $$",
            "SELECT p_gc()",
            "v",
        ),
        (
            "CREATE FUNCTION p_if() RETURNS int LANGUAGE plpgsql AS $$ DECLARE id int := 10; BEGIN IF EXISTS (SELECT 1 FROM t WHERE t.v = id) THEN RETURN 1; END IF; RETURN 0; END $$",
            "SELECT p_if()",
            "id",
        ),
        (
            "CREATE FUNCTION p_as() RETURNS int LANGUAGE plpgsql AS $$ DECLARE v int := 5; r int; BEGIN r := (SELECT max(v) FROM t); RETURN r; END $$",
            "SELECT p_as()",
            "v",
        ),
        (
            "CREATE FUNCTION p_rq() RETURNS SETOF int LANGUAGE plpgsql AS $$ DECLARE id int := 5; BEGIN RETURN QUERY SELECT id FROM t; END $$",
            "SELECT * FROM p_rq()",
            "id",
        ),
        (
            "CREATE FUNCTION p_for() RETURNS int LANGUAGE plpgsql AS $$ DECLARE id int := 5; r record; n int := 0; BEGIN FOR r IN SELECT id FROM t LOOP n := n + 1; END LOOP; RETURN n; END $$",
            "SELECT p_for()",
            "id",
        ),
        (
            "CREATE FUNCTION p_perf() RETURNS int LANGUAGE plpgsql AS $$ DECLARE id int := 5; BEGIN PERFORM id FROM t; RETURN 1; END $$",
            "SELECT p_perf()",
            "id",
        ),
        (
            "CREATE FUNCTION p_cur() RETURNS int LANGUAGE plpgsql AS $$ DECLARE id int := 5; c CURSOR FOR SELECT id FROM t; r record; BEGIN OPEN c; FETCH c INTO r; CLOSE c; RETURN r.id; END $$",
            "SELECT p_cur()",
            "id",
        ),
        // A parameter is a variable of the body too.
        (
            "CREATE FUNCTION p_pos(id int) RETURNS int LANGUAGE plpgsql AS $$ DECLARE r int; BEGIN SELECT id INTO r FROM t ORDER BY t.v LIMIT 1; RETURN r; END $$",
            "SELECT p_pos(3)",
            "id",
        ),
    ] {
        exec(&engine, definition);
        ambiguous(&engine, call, name);
    }
}

#[test]
fn a_name_only_one_of_them_takes_is_not_ambiguous() {
    let engine = tables();
    for (definition, call, expected) in [
        // An output column named in ORDER BY, GROUP BY or DISTINCT ON takes the name before the variable.
        (
            "CREATE FUNCTION p_ob() RETURNS int LANGUAGE plpgsql AS $$ DECLARE x int := 5; r int; BEGIN SELECT v AS x INTO r FROM t ORDER BY x DESC LIMIT 1; RETURN r; END $$",
            "SELECT p_ob()",
            Value::Int(2),
        ),
        (
            "CREATE FUNCTION p_gb() RETURNS bigint LANGUAGE plpgsql AS $$ DECLARE x int := 5; r bigint; BEGIN SELECT count(*) INTO r FROM (SELECT v AS x FROM t GROUP BY x) s; RETURN r; END $$",
            "SELECT p_gb()",
            Value::Int(2),
        ),
        (
            "CREATE FUNCTION p_dn() RETURNS bigint LANGUAGE plpgsql AS $$ DECLARE x int := 5; r bigint; BEGIN SELECT count(*) INTO r FROM (SELECT DISTINCT ON (x) v AS x FROM t) s; RETURN r; END $$",
            "SELECT p_dn()",
            Value::Int(2),
        ),
        // The rows of an INSERT see no relation.
        (
            "CREATE FUNCTION p_iv() RETURNS int LANGUAGE plpgsql AS $$ DECLARE id int := 42; BEGIN INSERT INTO tv VALUES (id, 1); RETURN (SELECT count(*) FROM tv)::int; END $$",
            "SELECT p_iv()",
            Value::Int(1),
        ),
        // A dynamic statement has no variables.
        (
            "CREATE FUNCTION p_dyn() RETURNS int LANGUAGE plpgsql AS $$ DECLARE id int := 5; r int; BEGIN EXECUTE 'SELECT id FROM t ORDER BY v LIMIT 1' INTO r; RETURN r; END $$",
            "SELECT p_dyn()",
            Value::Int(10),
        ),
    ] {
        exec(&engine, definition);
        assert_eq!(scalar(&engine, call), expected, "{definition}");
    }
}

#[test]
fn the_body_option_chooses_the_variable_or_the_column() {
    let engine = tables();
    exec(
        &engine,
        "CREATE FUNCTION p_uv() RETURNS int LANGUAGE plpgsql AS $$ #variable_conflict use_variable\nDECLARE id int := 5; r int; BEGIN SELECT id INTO r FROM t ORDER BY v LIMIT 1; RETURN r; END $$",
    );
    exec(
        &engine,
        "CREATE FUNCTION p_uc() RETURNS int LANGUAGE plpgsql AS $$ #variable_conflict use_column\nDECLARE id int := 5; r int; BEGIN SELECT id INTO r FROM t ORDER BY v LIMIT 1; RETURN r; END $$",
    );
    assert_eq!(scalar(&engine, "SELECT p_uv()"), Value::Int(5));
    assert_eq!(scalar(&engine, "SELECT p_uc()"), Value::Int(10));
    let error = exec_err(
        &engine,
        "CREATE FUNCTION p_bad() RETURNS int LANGUAGE plpgsql AS $$ #variable_conflict bogus\nBEGIN RETURN 1; END $$",
    );
    assert_eq!(error.sqlstate(), Some("42601"));
}

#[test]
fn the_setting_applies_when_a_session_compiles_the_body() {
    let engine = tables();
    let body = "DECLARE id int := 5; r int; BEGIN SELECT id INTO r FROM t ORDER BY v LIMIT 1; RETURN r; END";
    // CREATE FUNCTION compiles the body in its session under the setting of the moment, and the session keeps that compilation.
    exec(&engine, "SET plpgsql.variable_conflict = use_column");
    exec(
        &engine,
        &format!("CREATE FUNCTION q_guc() RETURNS int LANGUAGE plpgsql AS $$ {body} $$"),
    );
    assert_eq!(scalar(&engine, "SELECT q_guc()"), Value::Int(10));
    exec(&engine, "SET plpgsql.variable_conflict = error");
    assert_eq!(scalar(&engine, "SELECT q_guc()"), Value::Int(10));
    // The body's own option takes precedence over the setting.
    exec(&engine, "SET plpgsql.variable_conflict = use_column");
    exec(
        &engine,
        &format!(
            "CREATE FUNCTION q_opt() RETURNS int LANGUAGE plpgsql AS $$ #variable_conflict error\n{body} $$"
        ),
    );
    ambiguous(&engine, "SELECT q_opt()", "id");
    // A body CREATE FUNCTION left unexamined is compiled at its first call.
    exec(&engine, "SET check_function_bodies = off");
    exec(
        &engine,
        &format!("CREATE FUNCTION q_late() RETURNS int LANGUAGE plpgsql AS $$ {body} $$"),
    );
    exec(&engine, "SET check_function_bodies = on");
    exec(&engine, "SET plpgsql.variable_conflict = use_variable");
    assert_eq!(scalar(&engine, "SELECT q_late()"), Value::Int(5));
    // A routine's own SET clause applies when the body is compiled.
    exec(&engine, "SET plpgsql.variable_conflict = error");
    exec(
        &engine,
        &format!(
            "CREATE FUNCTION q_cfg() RETURNS int LANGUAGE plpgsql SET plpgsql.variable_conflict = use_variable AS $$ {body} $$"
        ),
    );
    assert_eq!(scalar(&engine, "SELECT q_cfg()"), Value::Int(5));
    // An anonymous block is compiled when it runs.
    exec(&engine, "SET plpgsql.variable_conflict = use_column");
    exec(&engine, "CREATE TABLE seen (id int)");
    exec(
        &engine,
        "DO $$ DECLARE id int := 5; BEGIN INSERT INTO seen SELECT id FROM t ORDER BY v LIMIT 1; END $$",
    );
    assert_eq!(scalar(&engine, "SELECT id FROM seen"), Value::Int(10));
}

#[test]
fn the_setting_is_defined_as_postgresql_defines_it() {
    let engine = tables();
    exec(
        &engine,
        "CREATE FUNCTION p_load() RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN 1; END $$",
    );
    let result = exec(
        &engine,
        "SELECT setting, context, vartype, enumvals::text, boot_val, short_desc FROM pg_settings WHERE name = 'plpgsql.variable_conflict'",
    );
    let row = &result.rows[0];
    assert_eq!(row["setting"], Value::Str("error".into()));
    assert_eq!(row["context"], Value::Str("superuser".into()));
    assert_eq!(row["vartype"], Value::Str("enum".into()));
    assert_eq!(
        row["enumvals"],
        Value::Str("{error,use_variable,use_column}".into())
    );
    assert_eq!(row["boot_val"], Value::Str("error".into()));
    assert_eq!(
        row["short_desc"],
        Value::Str(
            "Sets handling of conflicts between PL/pgSQL variable names and table column names."
                .into()
        )
    );
    let error = exec_err(&engine, "SET plpgsql.variable_conflict = bogus");
    assert_eq!(
        (error.sqlstate(), error.to_string()),
        (
            Some("22023"),
            "invalid value for parameter \"plpgsql.variable_conflict\": \"bogus\"".into()
        )
    );
}

#[test]
fn trigger_records_meet_the_old_and_new_aliases_only_in_returning() {
    let engine = tables();
    exec(&engine, "INSERT INTO tv VALUES (1, 10)");
    exec(&engine, "CREATE TABLE tl (id int, v int)");
    exec(&engine, "INSERT INTO tl VALUES (1, 0)");
    exec(
        &engine,
        "CREATE FUNCTION trf() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN UPDATE tl SET v = NEW.v WHERE id = OLD.id; RETURN NEW; END $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER trg BEFORE UPDATE ON tv FOR EACH ROW EXECUTE FUNCTION trf()",
    );
    exec(&engine, "UPDATE tv SET v = 11");
    assert_eq!(scalar(&engine, "SELECT v FROM tl"), Value::Int(11));
    exec(
        &engine,
        "CREATE OR REPLACE FUNCTION trf() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE r int; BEGIN UPDATE tl SET v = 5 WHERE id = OLD.id RETURNING new.v INTO r; RETURN NEW; END $$",
    );
    ambiguous(&engine, "UPDATE tv SET v = 12", "new.v");
}
