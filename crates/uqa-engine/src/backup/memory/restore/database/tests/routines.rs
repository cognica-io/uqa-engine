//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn complete_repository_input_rejects_missing_concrete_routines_before_private_activation() {
    for declaration in [
        "CREATE TABLE items(value integer DEFAULT f(1))",
        "CREATE TABLE items(value integer CHECK(f(value) > 0))",
        "CREATE TABLE items(value integer, computed integer GENERATED ALWAYS AS (f(value)) STORED)",
        "CREATE DOMAIN positive AS integer CHECK(f(VALUE) > 0)",
        "CREATE VIEW visible AS SELECT f(1) AS value",
        "CREATE VIEW visible AS SELECT * FROM f(1)",
        "CREATE VIEW visible AS SELECT * FROM ROWS FROM(f(1),f(2)) AS v(first,second)",
        "CREATE FUNCTION caller() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT f(1); END",
        "CREATE FUNCTION caller(n integer DEFAULT f(1)) RETURNS integer LANGUAGE SQL AS 'SELECT n'",
        "CREATE TABLE items(value integer); CREATE TABLE output(value integer); CREATE RULE saved AS ON INSERT TO items DO ALSO INSERT INTO output VALUES(f(NEW.value))",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source.sql("CREATE FUNCTION f(n integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS 'SELECT n+1'", &[]).unwrap();
        source.sql(declaration, &[]).unwrap();
        assert!(source.durable.sql_user_functions.write().remove("public.f").unwrap());
        let root = publish(&source, &repository);
        drop(source);
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&repository, root, &control);
        let state = loop {
            if let Some(state) = input.advance(1024).unwrap() { break state; }
        };
        drop(input);
        let Err(error) = state.into_private_engine(MemoryBudget::new(32 << 20), &control) else {
            panic!("missing routine reached private activation: {declaration}");
        };
        assert!(error.to_string().contains("invalid native routine reference"), "{declaration}: {error}");
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_repository_input_rejects_missing_trigger_routines_before_private_activation() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE TABLE items(value integer); CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER saved BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION fire()", &[]).unwrap();
    assert!(source
        .durable
        .sql_user_functions
        .write()
        .remove("public.fire")
        .unwrap());
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let state = loop {
        if let Some(state) = input.advance(1024).unwrap() {
            break state;
        }
    };
    drop(input);
    let Err(error) = state.into_private_engine(MemoryBudget::new(32 << 20), &control) else {
        panic!("missing trigger routine reached private activation");
    };
    assert!(
        error
            .to_string()
            .contains("invalid native routine reference"),
        "{error}"
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn complete_repository_input_retains_renamed_routines_and_lazy_source_bodies() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE FUNCTION f(n integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS 'SELECT n+1'; CREATE TABLE items(value integer DEFAULT f(1)); CREATE VIEW visible AS SELECT f(2) AS value; CREATE FUNCTION missing() RETURNS integer LANGUAGE SQL AS 'SELECT 7'; CREATE FUNCTION lazy() RETURNS integer LANGUAGE SQL AS 'SELECT missing()'; DROP FUNCTION missing(); ALTER FUNCTION f(integer) RENAME TO renamed", &[]).unwrap();
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let state = loop {
        if let Some(state) = input.advance(1024).unwrap() {
            break state;
        }
    };
    drop(input);
    let engine = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    assert_eq!(image(&engine), expected);
    engine.sql("INSERT INTO items DEFAULT VALUES", &[]).unwrap();
    assert_eq!(
        engine.sql("SELECT value FROM items", &[]).unwrap().rows[0]["value"],
        Value::Int(2)
    );
    assert_eq!(
        engine.sql("SELECT value FROM visible", &[]).unwrap().rows[0]["value"],
        Value::Int(3)
    );
    assert_eq!(
        engine.sql("SELECT lazy()", &[]).unwrap_err().sqlstate(),
        Some("42883")
    );
    drop(engine);
    assert_eq!(control.memory().used(), 0);
}
