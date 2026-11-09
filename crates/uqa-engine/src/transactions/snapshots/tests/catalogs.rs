//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

fn definitions(engine: &Engine) {
    use uqa_sql::catalog::range_type::{StoredMultirange, StoredRange};
    sql(engine, "CREATE SCHEMA saved; CREATE TABLE docs(id integer); INSERT INTO docs VALUES(1); CREATE DOMAIN saved.domain_type AS integer CHECK(VALUE > 0); CREATE TYPE saved.enum_type AS ENUM('first','second'); CREATE TYPE saved.composite_type AS (id integer, body text); CREATE TYPE saved.pending_type; CREATE VIEW saved.saved_view AS SELECT id FROM docs; CREATE FUNCTION saved.saved_function() RETURNS integer LANGUAGE SQL RETURN 7");
    engine
        .durable
        .ranges
        .write()
        .insert(
            "public.retained_range".into(),
            StoredRange {
                object_id: [1; 16],
                oid: 60_001,
                array_oid: 60_002,
                array_name: "_retained_range".into(),
                identity: RelationIdentity::new("public", "retained_range"),
                owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
                subtype: uqa_sql::ColumnType::Integer,
                subtype_operator_class: 1978,
                collation_oid: 0,
                canonical_oid: None,
                subtype_difference_oid: None,
                multirange: StoredMultirange {
                    object_id: [2; 16],
                    oid: 60_003,
                    array_oid: 60_004,
                    array_name: "_retained_multi".into(),
                    identity: RelationIdentity::new("public", "retained_multi"),
                },
                usage_acl: None,
            },
        )
        .unwrap();
}

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn transaction_catalog_sources_preserve_original_definitions_after_unread_retirement(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    definitions(&engine);
    let original_domains = engine.durable.domains.snapshot();
    let weak = original_domains.identity();
    let mut original = std::collections::BTreeMap::new();
    macro_rules! remember {
        ($($field:ident),+) => {$ (
            original.insert(stringify!($field), serde_json::to_value(engine.durable.$field.read().iter().collect::<Vec<_>>()).unwrap());
        )+};
    }
    remember!(domains, enums, composites, ranges, shells);
    let original_schemas = engine
        .durable
        .schemas
        .read()
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    let (view_id, view_query) = {
        let views = engine.durable.views.read();
        let view = &views[&RelationIdentity::new("saved", "saved_view")];
        (view.object_id, serde_json::to_value(&view.query).unwrap())
    };
    let routine_definition = serde_json::to_value(
        &engine.durable.sql_user_functions.read()["saved.saved_function"][0].def,
    )
    .unwrap();
    drop(original_domains);
    sql(&engine, "BEGIN; UPDATE docs SET id=2; ALTER TYPE saved.enum_type ADD VALUE 'private'; CREATE OR REPLACE FUNCTION saved.saved_function() RETURNS integer LANGUAGE SQL RETURN 99; CREATE OR REPLACE VIEW saved.saved_view AS SELECT id + 1 AS id FROM docs; SAVEPOINT nested; CREATE DOMAIN saved.private_type AS text; ROLLBACK TO nested");
    engine
        .durable
        .ranges
        .write()
        .edit("public.retained_range", |definition| {
            definition.array_name = "_private_array".into();
            Ok(())
        })
        .unwrap()
        .unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    let selected = capture(&engine, &control);
    assert!(selected
        .catalogs
        .budget()
        .shares_allowance(control.memory()));
    macro_rules! unread {
        ($($field:ident),+) => {$ (
            assert!(selected.catalogs.$field.retained(&selected.cut).unwrap().is_none());
        )+};
    }
    unread!(
        domains,
        enums,
        composites,
        ranges,
        shells,
        schemas,
        views,
        sql_user_functions
    );
    sql(&engine, completion);
    sql(&engine, "DROP SCHEMA saved CASCADE; CREATE SCHEMA saved; CREATE TYPE saved.enum_type AS ENUM('replacement'); DROP TABLE docs; CREATE TABLE docs(replacement boolean)");
    drop(engine);
    assert!(!weak.is_alive());
    macro_rules! exact {
        ($($field:ident),+) => {$ ({
            let captured = selected.catalogs.$field.captured(&selected.cut).unwrap();
            assert!(captured.budget().shares_allowance(control.memory()));
            assert_eq!(serde_json::to_value(captured.get().unwrap()).unwrap(), original[stringify!($field)]);
        })+};
    }
    exact!(domains, enums, composites, ranges, shells);
    let schemas = selected.catalogs.schemas.captured(&selected.cut).unwrap();
    assert!(schemas.budget().shares_allowance(control.memory()));
    assert!(schemas.get().unwrap().iter().eq(original_schemas
        .iter()
        .map(|(name, security)| (name, security))));
    drop(schemas);
    let views = selected.catalogs.views.captured(&selected.cut).unwrap();
    assert_eq!(views.get().unwrap().len(), 1);
    let (_, saved_view) = views.get().unwrap().iter().next().unwrap();
    assert_eq!(saved_view.object_id, view_id);
    assert_eq!(serde_json::to_value(&saved_view.query).unwrap(), view_query);
    drop(views);
    let routines = selected
        .catalogs
        .sql_user_functions
        .captured(&selected.cut)
        .unwrap();
    let saved = routines.get().unwrap();
    let saved = saved
        .iter()
        .find(|(name, _)| *name == "saved.saved_function")
        .unwrap();
    assert_eq!(
        serde_json::to_value(&saved.1[0].def).unwrap(),
        routine_definition
    );
    assert!(matches!(
        saved.1[0].body,
        uqa_sql::routines::RoutineBody::Bound(_)
    ));
    assert_eq!(
        selected
            .table(&RelationIdentity::new("public", "docs"))
            .unwrap()
            .document_store
            .get_field(1, "id")
            .unwrap(),
        Some(Value::Int(1))
    );
    drop(selected);
    control.cancellation().cancel();
    assert!(routines.get().is_err());
    drop(routines);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn transaction_catalog_sources_exclude_temporary_definitions_during_a_private_callback() {
    let engine = Arc::new(Engine::new());
    definitions(&engine);
    sql(&engine, "CREATE TEMP TABLE temp_table(id integer); CREATE FUNCTION pg_temp.temp_function() RETURNS text LANGUAGE SQL AS 'SELECT ''temporary'''; CREATE TEMP VIEW temp_view AS SELECT id FROM temp_table");
    sql(
        &engine,
        &format!(
            "CREATE DOMAIN pg_temp.temp_domain AS text DEFAULT '{}'",
            "t".repeat(1 << 20)
        ),
    );
    let (entered, started) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let writer = engine.clone();
    let worker = std::thread::spawn(move || {
        writer.transaction(|engine| {
        sql(engine, "UPDATE docs SET id=9; CREATE TYPE saved.private_type AS ENUM('private'); SAVEPOINT nested");
        entered.send(()).unwrap();
        proceed.recv().unwrap();
        Ok(())
    })
    });
    started
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let selected = capture(&engine, &control);
    let domains = selected.catalogs.domains.captured(&selected.cut).unwrap();
    assert_eq!(domains.get().unwrap().len(), 1);
    assert!(domains.get().unwrap().get("saved.domain_type").is_some());
    let enums = selected.catalogs.enums.captured(&selected.cut).unwrap();
    assert_eq!(enums.get().unwrap().len(), 1);
    assert_eq!(
        enums
            .get()
            .unwrap()
            .get("saved.enum_type")
            .unwrap()
            .labels
            .len(),
        2
    );
    let routines = selected
        .catalogs
        .sql_user_functions
        .captured(&selected.cut)
        .unwrap();
    assert!(routines
        .get()
        .unwrap()
        .iter()
        .all(|(name, _)| !name.starts_with("pg_temp_")));
    assert_eq!(
        selected
            .catalogs
            .views
            .captured(&selected.cut)
            .unwrap()
            .get()
            .unwrap()
            .len(),
        1
    );
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    drop((engine, selected, domains, enums, routines));
    assert_eq!(control.memory().used(), 0);
}
