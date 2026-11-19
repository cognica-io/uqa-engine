//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn complete_repository_input_rejects_cross_catalog_name_collisions() {
    for collision in ["view", "array", "index"] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source.sql("CREATE TABLE items(value integer); CREATE VIEW visible AS SELECT 1 AS value; CREATE TYPE mood AS ENUM('low','high'); CREATE INDEX saved ON items(value)", &[]).unwrap();
        let items = uqa_core::RelationIdentity::new("public", "items");
        match collision {
            "view" => {
                let mut views = source.durable.views.write();
                let original = uqa_core::RelationIdentity::new("public", "visible");
                let view = views.get(&original).unwrap().clone();
                views.remove(&original).unwrap();
                views.insert(items, view).unwrap();
            }
            "array" => {
                let mut enums = source.durable.enums.write();
                let mut definition = enums.get("public.mood").unwrap().clone();
                definition.array_name = "items".into();
                enums.insert("public.mood".into(), definition).unwrap();
            }
            "index" => {
                let mut indexes = source.durable.catalog_indexes.write();
                let original = uqa_core::RelationIdentity::new("public", "saved");
                let mut index = indexes.get(&original).unwrap().clone();
                indexes.remove(&original).unwrap();
                index.relation = items;
                indexes
                    .insert(index, uqa_sql::ast::RelationPersistence::Permanent)
                    .unwrap();
            }
            _ => unreachable!(),
        }
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
            panic!("cross-catalog name collision reached private activation: {collision}");
        };
        assert!(
            error.to_string().contains("invalid native catalog name"),
            "{collision}: {error}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_repository_input_preserves_separate_namespaces_and_displaced_array_names() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE TABLE items(value integer); CREATE INDEX reusable ON items(value); CREATE DOMAIN reusable AS integer CHECK(VALUE > 0); CREATE TYPE pair AS (value integer); CREATE TABLE _pair(value integer); CREATE TABLE _items(value integer); CREATE TABLE typed(value reusable, pair pair); INSERT INTO typed VALUES(7,ROW(9)::pair)", &[]).unwrap();
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
    assert_eq!(
        engine.sql("SELECT value FROM typed", &[]).unwrap().rows[0]["value"],
        Value::Int(7)
    );
    engine
        .sql(
            "INSERT INTO typed VALUES(11,ROW(13)::pair); INSERT INTO items VALUES(17)",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) AS count FROM typed", &[])
            .unwrap()
            .rows[0]["count"],
        Value::Int(2)
    );
    assert_eq!(
        engine
            .sql("SELECT value FROM items WHERE value=17", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(17)
    );
    drop(engine);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn complete_repository_input_rejects_missing_catalog_namespaces() {
    for declaration in [
        "CREATE TABLE app.items(value integer)",
        "CREATE UNLOGGED TABLE app.items(value integer)",
        "CREATE VIEW app.visible AS SELECT 1 AS value",
        "CREATE MATERIALIZED VIEW app.visible AS SELECT 1 AS value",
        "CREATE SEQUENCE app.ticket",
        "CREATE DOMAIN app.positive AS integer CHECK(VALUE > 0)",
        "CREATE TYPE app.mood AS ENUM('low','high')",
        "CREATE TYPE app.pair AS (value integer)",
        "CREATE TYPE app.pending",
        "CREATE FUNCTION app.f() RETURNS integer LANGUAGE SQL AS 'SELECT 1'",
        "CREATE PROCEDURE app.p() LANGUAGE SQL AS 'SELECT 1'",
        "CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE app.items(value integer) SERVER remote",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source.sql("CREATE SCHEMA app", &[]).unwrap();
        source.sql(declaration, &[]).unwrap();
        assert!(source.durable.schemas.write().remove("app").unwrap());
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
            panic!("missing namespace reached private activation: {declaration}");
        };
        assert!(error.to_string().contains("missing native catalog namespace"), "{declaration}: {error}");
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_repository_input_preserves_quoted_catalog_namespaces() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql(r#"CREATE SCHEMA "app.""quoted"; CREATE TABLE "app.""quoted".items(value integer); INSERT INTO "app.""quoted".items VALUES(7); CREATE FUNCTION "app.""quoted".f() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT value FROM "app.""quoted".items; END; CREATE VIEW "app.""quoted".visible AS SELECT "app.""quoted".f() AS value"#, &[]).unwrap();
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
    assert_eq!(
        engine
            .sql(r#"SELECT value FROM "app.""quoted".visible"#, &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(7)
    );
    engine
        .sql(r#"UPDATE "app.""quoted".items SET value=9"#, &[])
        .unwrap();
    assert_eq!(
        engine
            .sql(r#"SELECT value FROM "app.""quoted".visible"#, &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(9)
    );
    drop(engine);
    assert_eq!(control.memory().used(), 0);
}
