//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn routine(source: &Engine, statements: usize) {
    let body = (1..=statements)
        .map(|number| format!("SELECT {number};"))
        .collect::<Vec<_>>()
        .join(" ");
    source
        .sql(
            &format!("CREATE FUNCTION public.stepped_body() RETURNS integer LANGUAGE sql BEGIN ATOMIC {body} END"),
            &[],
        )
        .unwrap();
}

fn body_steps(source: &Engine, answer: i64) -> usize {
    query_steps(source, "SELECT stepped_body() AS v", &Value::Int(answer))
}

fn query_steps(source: &Engine, query: &str, answer: &Value) -> usize {
    let control = StorageReadControl::with_limit(32 << 20);
    let mut candidate = input(source, &control);
    reach_bodies(&mut candidate);
    let mut steps = 0;
    while compiling(&candidate) {
        assert!(candidate.advance(1).unwrap().is_none());
        steps += 1;
    }
    let restored = finish(&mut candidate).finish().unwrap();
    assert_eq!(&restored.sql(query, &[]).unwrap().rows[0]["v"], answer);
    drop((restored, candidate));
    assert_eq!(control.memory().used(), 0);
    steps
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_sql_body_advances_through_each_statement(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let short = provider.open(&directory.path().join("short"));
    let long = provider.open(&directory.path().join("long"));
    routine(&short, 1);
    routine(&long, 17);
    let short_steps = body_steps(&short, 1);
    let long_steps = body_steps(&long, 17);
    eprintln!("one statement took {short_steps} body steps, seventeen took {long_steps}");
    assert!(
        long_steps >= short_steps + 16,
        "one statement took {short_steps} body steps, seventeen took {long_steps}"
    );
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_sql_compiler_advances_within_one_values_statement(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let measure = |rows: usize| {
        let source = provider.open(&directory.path().join(rows.to_string()));
        let values = std::iter::repeat_n("(1)", rows)
            .collect::<Vec<_>>()
            .join(",");
        source.sql(&format!(
            "CREATE FUNCTION counted(delta bigint) RETURNS bigint LANGUAGE SQL BEGIN ATOMIC SELECT count(*) + delta FROM (VALUES {values}) AS items(n); END"
        ), &[]).unwrap();
        let expected = Value::Int(i64::try_from(rows).unwrap() + 7);
        query_steps(&source, "SELECT counted(7) AS v", &expected)
    };
    let short = measure(1);
    let wide = measure(128);
    assert!(
        wide > short + 128,
        "{provider:?}: one row {short}, 128 rows {wide}"
    );
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_configuration_advances_through_large_effective_values(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let steps = |file: &str, value: &str| {
        let source = provider.open(&directory.path().join(file));
        source.sql(&format!(
            "CREATE FUNCTION public.stepped_body() RETURNS integer LANGUAGE sql SET my.retained = '{value}' AS $$ SELECT length(current_setting('my.retained')) $$"
        ), &[]).unwrap();
        body_steps(&source, i64::try_from(value.chars().count()).unwrap())
    };
    let short = steps("short", "a");
    let wide = steps("wide", &"a𐐷".repeat(40960));
    assert!(
        wide >= short + 4,
        "{provider:?}: {short} short versus {wide} wide body steps"
    );
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_routine_result_type_names_yield_before_binding(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    for (form, body, query) in [
        ("", "RETURN NULL", "SELECT saved() IS NULL AS v"),
        ("SETOF ", "RETURN", "SELECT count(*) = 0 AS v FROM saved()"),
    ] {
        let measure = |dimensions| {
            let source = provider.open(&directory.path().join(format!("{form}{dimensions}")));
            let brackets = "[]".repeat(dimensions);
            source.sql(&format!(
                "CREATE FUNCTION saved() RETURNS {form}integer{brackets} LANGUAGE plpgsql AS $$BEGIN {body}; END;$$"
            ), &[]).unwrap();
            query_steps(&source, query, &Value::Bool(true))
        };
        let short = measure(1);
        let wide = measure(4096);
        assert!(
            wide > short,
            "{provider:?} {form} short={short} wide={wide}"
        );
    }
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn partial_sql_body_analysis_releases_its_registry_and_allowance(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("partial"));
    source
        .sql(
            "CREATE SCHEMA body_path; SET search_path = body_path, public",
            &[],
        )
        .unwrap();
    routine(&source, 17);
    source.sql("RESET search_path", &[]).unwrap();
    let catalog = source.storage.catalog.as_ref().unwrap();
    let definitions = catalog.get_metadata("sql_functions_json").unwrap();
    let halfway = body_steps(&source, 17) / 2;
    for cancel in [true, false] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut candidate = input(&source, &control);
        reach_bodies(&mut candidate);
        let private = candidate.candidate.as_ref().unwrap().session.engine_owner();
        let lifetime = Arc::downgrade(private);
        let path = private.session.state.read().search_path.clone();
        for _ in 0..halfway {
            assert!(candidate.advance(1).unwrap().is_none());
            assert!(compiling(&candidate));
            let engine = candidate.candidate.as_ref().unwrap().session.engine();
            assert!(Arc::ptr_eq(&path, &engine.session.state.read().search_path));
        }
        let engine = candidate.candidate.as_ref().unwrap().session.engine();
        let registry = engine.durable.sql_user_functions.read();
        let RoutineBody::Bound(body) = &registry["public.stepped_body"][0].body else {
            panic!("missing placeholder")
        };
        assert!(matches!(&***body, CompiledFunctionBody::SQL(plans) if plans.is_empty()));
        drop(registry);
        let backend = Arc::clone(engine.storage.backend.as_ref().unwrap());
        let occupied = if cancel {
            control.cancellation().cancel();
            None
        } else {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        };
        loop {
            match candidate.advance(1) {
                Ok(None) => assert!(compiling(&candidate)),
                Ok(Some(_)) => panic!("partial routine was published"),
                Err(error) => {
                    if cancel {
                        assert!(
                            matches!(error, StorageBackendError::Cancelled(_)),
                            "{error}"
                        );
                    } else {
                        assert!(
                            matches!(
                                error,
                                StorageBackendError::Memory(
                                    uqa_core::memory::MemoryError::Limit { .. }
                                )
                            ),
                            "{error}"
                        );
                    }
                    break;
                }
            }
        }
        drop((candidate, occupied));
        assert!(lifetime.upgrade().is_none());
        assert!(!backend.in_transaction());
        assert_eq!(control.memory().used(), 0);
        assert_eq!(
            catalog.get_metadata("sql_functions_json").unwrap(),
            definitions
        );
    }
    let control = StorageReadControl::with_limit(32 << 20);
    let mut candidate = input(&source, &control);
    while !retiring(&candidate) {
        assert!(candidate.advance(1).unwrap().is_none());
    }
    assert!(candidate.advance(1).unwrap().is_none());
    assert!(retiring(&candidate));
    let private = candidate.candidate.as_ref().unwrap().session.engine_owner();
    let lifetime = Arc::downgrade(private);
    let backend = Arc::clone(private.storage.backend.as_ref().unwrap());
    control.cancellation().cancel();
    assert!(matches!(
        candidate.advance(1),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(lifetime.upgrade().is_none());
    assert!(!backend.in_transaction());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(
        catalog.get_metadata("sql_functions_json").unwrap(),
        definitions
    );
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn routine_statement_errors_keep_postgresql_order_during_creation_and_restore(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let column = "SELECT missing_body_column";
    let relation = "SELECT * FROM public.missing_body_relation";
    for (first, second, state, message) in [
        (
            column,
            relation,
            "42703",
            "column \"missing_body_column\" does not exist",
        ),
        (
            relation,
            column,
            "42P01",
            "relation \"public.missing_body_relation\" does not exist",
        ),
    ] {
        let source = provider.open(&directory.path().join(state));
        routine(&source, 1);
        let catalog = source.storage.catalog.as_ref().unwrap();
        let original = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
        let sql = format!("CREATE FUNCTION diagnostic_order() RETURNS integer LANGUAGE sql BEGIN ATOMIC SELECT 1; {first}; {second}; END");
        let error = source.sql(&sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{error}");
        assert!(error.to_string().contains(message), "{error}");
        let uqa_sql::Statement::CreateFunction(definition) =
            uqa_sql::compile(&sql).unwrap().remove(0)
        else {
            unreachable!()
        };
        let mut document: serde_json::Value = serde_json::from_str(&original).unwrap();
        document["definitions"]["public.stepped_body"][0]["body"] =
            serde_json::to_value(definition.body).unwrap();
        catalog
            .set_metadata("sql_functions_json", &document.to_string())
            .unwrap();
        let control = StorageReadControl::with_limit(32 << 20);
        let mut candidate = input(&source, &control);
        reach_bodies(&mut candidate);
        loop {
            match candidate.advance(1) {
                Ok(None) => assert!(compiling(&candidate)),
                Ok(Some(_)) => panic!("invalid routine was published"),
                Err(error) => {
                    let StorageBackendError::CatalogBinding(ref diagnostic) = error else {
                        panic!("expected retained SQL diagnostic: {error}")
                    };
                    let sql = diagnostic
                        .source()
                        .unwrap()
                        .downcast_ref::<uqa_sql::SQLError>()
                        .unwrap();
                    assert_eq!(sql.sqlstate(), Some(state));
                    assert_eq!(error.to_string(), message);
                    break;
                }
            }
        }
        drop(candidate);
        assert_eq!(control.memory().used(), 0);
    }
}
