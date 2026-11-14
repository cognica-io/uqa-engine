//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::authority_input::PersistentAuthorityInput;
use super::*;

fn reach_builtins(input: &mut PersistentEngineInput) {
    for _ in 0..65536 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Authority(PersistentAuthorityInput::Builtins { .. })
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("builtin authority stage was not reached");
}

fn reach_database(input: &mut PersistentEngineInput) {
    for _ in 0..65536 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Authority(PersistentAuthorityInput::Database { .. })
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("database authority stage was not reached");
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_builtin_authority_keeps_selected_roots_and_later_sql_mutations(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("authority"));
    source.sql("CREATE ROLE reader; REVOKE EXECUTE ON FUNCTION lower(text) FROM PUBLIC; GRANT EXECUTE ON FUNCTION lower(text) TO reader; CREATE FUNCTION normalized() RETURNS text LANGUAGE SQL AS $$ SELECT lower('ABC') $$", &[]).unwrap();
    let expected = source.durable.builtin_routine_security.read().clone();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_builtins(&mut input);
    assert!(input.advance(1).unwrap().is_none());
    source
        .sql("REVOKE EXECUTE ON FUNCTION lower(text) FROM reader", &[])
        .unwrap();
    let session = finish(&mut input);
    assert_eq!(
        *session.engine().durable.builtin_routine_security.read(),
        expected
    );
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.sql("SET ROLE reader", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT lower('ABC')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    restored
        .sql(
            "RESET ROLE; GRANT EXECUTE ON FUNCTION lower(text) TO reader; SET ROLE reader",
            &[],
        )
        .unwrap();
    assert_eq!(
        restored
            .sql("SELECT normalized() AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Str("abc".into())
    );
    restored.sql("RESET ROLE; BEGIN; REVOKE EXECUTE ON FUNCTION lower(text) FROM reader; ROLLBACK; SET ROLE reader", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT lower('DEF') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Str("def".into())
    );
    drop((restored, input, source, expected));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_builtin_authority_retires_private_roots_and_transactions(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("authority"));
    for index in 0..32 {
        source.sql(&format!("CREATE ROLE reader_{index}; GRANT EXECUTE ON FUNCTION lower(text) TO reader_{index} WITH GRANT OPTION"), &[]).unwrap();
    }
    let catalog = source.storage.catalog.as_ref().unwrap();
    let before = catalog.metadata_with_prefix("").unwrap();
    for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_builtins(&mut input);
        assert!(input.advance(64).unwrap().is_none());
        assert!(matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Authority(PersistentAuthorityInput::Builtins { .. })
        ));
        let backend = Arc::clone(
            input
                .candidate
                .as_ref()
                .unwrap()
                .session
                .engine()
                .storage
                .backend
                .as_ref()
                .unwrap(),
        );
        let occupied = (terminal == "memory").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        match terminal {
            "cancel" => control.cancellation().cancel(),
            "unwind" => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _owner = input;
                    panic!("retire authority");
                }));
                assert!(!backend.in_transaction());
                assert_eq!(control.memory().used(), 0);
                continue;
            }
            _ => (),
        }
        if terminal != "drop" {
            loop {
                match input.advance(usize::from(terminal != "zero")) {
                    Err(_) => break,
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("terminal authority input completed"),
                }
            }
            assert!(input.advance(1).is_err());
            assert!(input.candidate.is_none());
        }
        drop((input, occupied));
        assert!(!backend.in_transaction());
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
    source
        .sql("GRANT EXECUTE ON FUNCTION lower(text) TO PUBLIC", &[])
        .unwrap();
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_database_authority_retires_private_roots_and_transactions(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("authority"));
    for index in 0..32 {
        source.sql(&format!("CREATE ROLE reader_{index}; GRANT CREATE ON DATABASE uqa TO reader_{index} WITH GRANT OPTION"), &[]).unwrap();
    }
    let catalog = source.storage.catalog.as_ref().unwrap();
    let before = catalog.metadata_with_prefix("").unwrap();
    for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_database(&mut input);
        assert!(input.advance(64).unwrap().is_none());
        assert!(matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Authority(PersistentAuthorityInput::Database { .. })
        ));
        let backend = Arc::clone(
            input
                .candidate
                .as_ref()
                .unwrap()
                .session
                .engine()
                .storage
                .backend
                .as_ref()
                .unwrap(),
        );
        let occupied = (terminal == "memory").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        match terminal {
            "cancel" => control.cancellation().cancel(),
            "unwind" => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _owner = input;
                    panic!("retire authority");
                }));
                assert!(!backend.in_transaction());
                assert_eq!(control.memory().used(), 0);
                continue;
            }
            _ => (),
        }
        if terminal != "drop" {
            loop {
                match input.advance(usize::from(terminal != "zero")) {
                    Err(_) => break,
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("terminal authority input completed"),
                }
            }
            assert!(input.advance(1).is_err());
            assert!(input.candidate.is_none());
        }
        drop((input, occupied));
        assert!(!backend.in_transaction());
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
    source
        .sql("GRANT CREATE ON DATABASE uqa TO PUBLIC", &[])
        .unwrap();
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_database_authority_keeps_selected_roots_and_later_sql_mutations(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("database"));
    source
        .sql(
            "CREATE ROLE reader; GRANT CREATE ON DATABASE uqa TO reader",
            &[],
        )
        .unwrap();
    let expected = source.durable.database_security.read().clone();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_database(&mut input);
    assert!(input.advance(1).unwrap().is_none());
    source
        .sql("REVOKE CREATE ON DATABASE uqa FROM reader", &[])
        .unwrap();
    let session = finish(&mut input);
    assert_eq!(*session.engine().durable.database_security.read(), expected);
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.sql("SET ROLE reader", &[]).unwrap();
    assert_eq!(
        restored
            .sql("CREATE SCHEMA unavailable", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    restored.sql("RESET ROLE; GRANT CREATE ON DATABASE uqa TO reader; SET ROLE reader; CREATE SCHEMA admitted",&[]).unwrap();
    restored.sql("RESET ROLE; BEGIN; REVOKE CREATE ON DATABASE uqa FROM reader; ROLLBACK; SET ROLE reader; CREATE SCHEMA after_rollback",&[]).unwrap();
    assert!(restored.durable.schemas.read().contains_key("admitted"));
    assert!(restored
        .durable
        .schemas
        .read()
        .contains_key("after_rollback"));
    drop((restored, input, source, expected));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}
