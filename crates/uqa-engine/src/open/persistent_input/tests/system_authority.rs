//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::authority_input::PersistentAuthorityInput;
use super::*;

fn reach_system(input: &mut PersistentEngineInput) {
    for _ in 0..65536 {
        if matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Authority(PersistentAuthorityInput::System { .. })
        ) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("system authority stage was not reached");
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_system_authority_keeps_selected_tuples_and_later_sql_mutations(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("system_authority"));
    source
        .sql(
            "CREATE ROLE reader LOGIN; GRANT SELECT (usename) ON pg_catalog.pg_shadow TO reader",
            &[],
        )
        .unwrap();
    assert!(!source
        .sql("SELECT usename FROM pg_catalog.pg_shadow", &[])
        .unwrap()
        .rows
        .is_empty());
    let expected = source.durable.system_relation_security.read().clone();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_system(&mut input);
    assert!(input.advance(1).unwrap().is_none());
    source
        .sql(
            "REVOKE SELECT (usename) ON pg_catalog.pg_shadow FROM reader",
            &[],
        )
        .unwrap();
    let session = finish(&mut input);
    assert_eq!(
        *session.engine().durable.system_relation_security.read(),
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
            .sql("SELECT usename FROM pg_catalog.pg_shadow", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    restored
        .sql(
            "RESET ROLE; GRANT SELECT (usename) ON pg_catalog.pg_shadow TO reader; SET ROLE reader",
            &[],
        )
        .unwrap();
    assert!(!restored
        .sql("SELECT usename FROM pg_catalog.pg_shadow", &[])
        .unwrap()
        .rows
        .is_empty());
    restored.sql("RESET ROLE; BEGIN; REVOKE SELECT (usename) ON pg_catalog.pg_shadow FROM reader; ROLLBACK; SET ROLE reader", &[]).unwrap();
    assert!(!restored
        .sql("SELECT usename FROM pg_catalog.pg_shadow", &[])
        .unwrap()
        .rows
        .is_empty());
    drop((restored, input, source, expected));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_system_authority_retires_private_roots_and_transactions(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("authority"));
    for index in 0..32 {
        source.sql(&format!("CREATE ROLE reader_{index}; GRANT SELECT (usename) ON pg_catalog.pg_shadow TO reader_{index} WITH GRANT OPTION"), &[]).unwrap();
    }
    let catalog = source.storage.catalog.as_ref().unwrap();
    let before = catalog.metadata_with_prefix("").unwrap();
    for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_system(&mut input);
        assert!(input.advance(64).unwrap().is_none());
        assert!(matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Authority(PersistentAuthorityInput::System { .. })
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
        .sql(
            "GRANT SELECT (usename) ON pg_catalog.pg_shadow TO PUBLIC",
            &[],
        )
        .unwrap();
}
