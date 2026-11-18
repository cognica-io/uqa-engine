//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::authority_input::PersistentAuthorityInput;
use super::*;
use uqa_sql::routines::{CompiledFunctionBody, RoutineBody};

fn reading_routines(input: &PersistentEngineInput) -> bool {
    matches!(
        input.candidate.as_ref().unwrap().stage,
        Stage::Authority(PersistentAuthorityInput::Routines { .. })
    )
}

fn reach_routines(input: &mut PersistentEngineInput) {
    while !reading_routines(input) {
        assert!(input.advance(1).unwrap().is_none());
    }
}

fn compiling(input: &PersistentEngineInput) -> bool {
    matches!(
        input.candidate.as_ref().unwrap().stage,
        Stage::Registries(RegistryInput::Bodies(_))
    )
}

fn reach_bodies(input: &mut PersistentEngineInput) {
    while !compiling(input) {
        assert!(input.advance(1).unwrap().is_none());
    }
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_routines_publish_complete_placeholders_and_finalize_after_views(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("routines"));
    source.sql(
        "CREATE DOMAIN restored_arg AS integer;
         CREATE TABLE restored_data(id integer);
         INSERT INTO restored_data VALUES(7);
         CREATE VIEW restored_view AS SELECT id FROM restored_data;
         CREATE FUNCTION public.z_value() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT id FROM restored_view; END;
         CREATE FUNCTION public.a_value(v restored_arg) RETURNS integer LANGUAGE SQL RETURN public.z_value() + v;
         CREATE FUNCTION public.a_value(v text) RETURNS text LANGUAGE SQL AS $$ SELECT v $$",
        &[],
    ).unwrap();
    let original = source.durable.sql_user_functions.read().clone();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_routines(&mut input);
    assert!(input.advance(32).unwrap().is_none());
    assert!(reading_routines(&input));
    assert!(input
        .candidate
        .as_ref()
        .unwrap()
        .session
        .engine()
        .durable
        .sql_user_functions
        .read()
        .is_empty());
    source
        .sql(
            "CREATE FUNCTION added_later() RETURNS integer LANGUAGE SQL RETURN 9",
            &[],
        )
        .unwrap();
    while reading_routines(&input) {
        assert!(input.advance(1).unwrap().is_none());
    }
    let placeholders = input
        .candidate
        .as_ref()
        .unwrap()
        .session
        .engine()
        .durable
        .sql_user_functions
        .read()
        .clone();
    assert!(!placeholders.contains_key("public.added_later"));
    for (name, group) in original.iter() {
        let restored = &placeholders[name.as_str()];
        assert_eq!(restored.len(), group.len());
        for (placeholder, expected) in restored.iter().zip(group) {
            assert_eq!(placeholder.catalog_revision(), expected.catalog_revision());
            assert!(placeholder.is_retained_by(control.memory()));
            let RoutineBody::Bound(body) = &placeholder.body else {
                panic!("missing placeholder")
            };
            assert!(matches!(&***body, CompiledFunctionBody::SQL(plans) if plans.is_empty()));
        }
    }
    reach_bodies(&mut input);
    let mut polls = 0;
    while compiling(&input) {
        let functions = input
            .candidate
            .as_ref()
            .unwrap()
            .session
            .engine()
            .durable
            .sql_user_functions
            .read();
        for (name, group) in placeholders.iter() {
            assert!(functions[name.as_str()]
                .iter()
                .zip(group)
                .all(|(a, b)| Arc::ptr_eq(a, b)));
        }
        drop(functions);
        assert!(input.advance(1).unwrap().is_none());
        polls += 1;
    }
    assert!(polls > original.values().map(Vec::len).sum::<usize>());
    let session = finish(&mut input);
    {
        let functions = session.engine().durable.sql_user_functions.read();
        assert!(!functions.contains_key("public.added_later"));
        let routine = &functions["public.z_value"][0];
        let RoutineBody::Bound(body) = &routine.body else {
            panic!("unbound routine")
        };
        assert!(matches!(&***body, CompiledFunctionBody::SQL(plans) if !plans.is_empty()));
        assert!(routine.is_retained_by(control.memory()));
        assert!(!functions.budget().shares_allowance(control.memory()));
    }
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    assert_eq!(
        restored
            .sql("SELECT public.a_value(5::restored_arg) AS v", &[])
            .unwrap()
            .rows[0]["v"],
        Value::Int(12)
    );
    assert_eq!(
        restored
            .sql("SELECT public.a_value('ok'::text) AS v", &[])
            .unwrap()
            .rows[0]["v"],
        Value::Str("ok".into())
    );
    restored
        .sql("INSERT INTO restored_data VALUES(8)", &[])
        .unwrap();
    assert_eq!(
        restored
            .sql("SELECT count(*) AS n FROM restored_view", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
    restored.sql("CREATE OR REPLACE FUNCTION public.a_value(v text) RETURNS text LANGUAGE SQL RETURN upper(v)", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT public.a_value('ok'::text) AS v", &[])
            .unwrap()
            .rows[0]["v"],
        Value::Str("OK".into())
    );
    drop((placeholders, input, restored, occupied));
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_routine_input_retires_partial_declarations_and_the_private_transaction(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("cancel-routines"));
    source
        .sql(
            "CREATE FUNCTION never_partial(v integer) RETURNS integer LANGUAGE SQL RETURN v + 1",
            &[],
        )
        .unwrap();
    let catalog = source.storage.catalog.as_ref().unwrap();
    let before = catalog.get_metadata("sql_functions_json").unwrap();
    for (terminal, bodies) in ["cancel", "zero", "memory", "drop"]
        .into_iter()
        .flat_map(|terminal| [(terminal, false), (terminal, true)])
    {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        if bodies {
            reach_bodies(&mut input);
            assert!(input.advance(1).unwrap().is_none());
            assert!(compiling(&input));
        } else {
            reach_routines(&mut input);
            assert!(input.advance(16).unwrap().is_none());
            assert!(reading_routines(&input));
        }
        let engine = input.candidate.as_ref().unwrap().session.engine();
        let backend = Arc::clone(engine.storage.backend.as_ref().unwrap());
        let native = engine.durable.sql_user_functions.read().budget().clone();
        let occupied = (terminal == "memory").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        if terminal == "cancel" {
            control.cancellation().cancel();
        }
        if terminal != "drop" {
            loop {
                match input.advance(usize::from(terminal != "zero")) {
                    Ok(None) => (),
                    Err(_) => break,
                    Ok(Some(_)) => panic!("partial routine input was published"),
                }
            }
            assert!(input.advance(1).is_err());
        }
        drop((input, occupied));
        assert!(!backend.in_transaction());
        assert_eq!(native.used(), 0);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(catalog.get_metadata("sql_functions_json").unwrap(), before);
    }
}
