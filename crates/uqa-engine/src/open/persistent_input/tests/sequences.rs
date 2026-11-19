//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn reach_sequences(input: &mut PersistentEngineInput) {
    for _ in 0..65536 {
        if matches!(input.candidate.as_ref().unwrap().stage, Stage::Sequences(_)) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("sequence input was not reached")
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_sequence_input_keeps_selected_definitions_values_authority_and_live_mutations(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("sequences"));
    source.sql("CREATE ROLE reader; CREATE SEQUENCE ids START 17 INCREMENT 3; GRANT USAGE ON SEQUENCE ids TO reader; CREATE TABLE items(id integer GENERATED ALWAYS AS IDENTITY); SELECT nextval('ids')", &[]).unwrap();
    let original = source
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_sequence_rows()
        .unwrap();
    let authority = source.durable.sequences.read().security().clone();
    let oids = uqa_execution::catalog::sequence::catalog_oids::load(
        source.storage.catalog.as_ref().unwrap().as_ref(),
    )
    .unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_sequences(&mut input);
    assert!(input.advance(3).unwrap().is_none());
    source
        .sql(
            "ALTER SEQUENCE ids RENAME TO later; SELECT nextval('later')",
            &[],
        )
        .unwrap();
    let session = finish(&mut input);
    {
        let values = session.engine().durable.sequences.read();
        assert_eq!(values.len(), original.len());
        assert_eq!(values.security(), &authority);
        assert_eq!(
            values.catalog_oids().iter().collect::<Vec<_>>(),
            oids.iter().collect::<Vec<_>>()
        );
        for row in original {
            let object = row.object_id;
            let (relation, state) =
                uqa_execution::catalog::sequence::restoration::sequence_state_from_row(row)
                    .unwrap();
            assert_eq!(values.object_id(&relation), Some(&object));
            assert_eq!(values.state(&relation), Some(&state));
        }
    }
    let mut restored = session.finish().unwrap();
    // Complete the same provider attachment as the restored-provider constructor before issuing live sequence operations.
    restored
        .activate_persistent_provider(source.storage.provider.as_ref().unwrap())
        .unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.sql("SET ROLE reader", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT nextval('later') AS n", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(23)
    );
    restored.sql("RESET ROLE; INSERT INTO items DEFAULT VALUES; CREATE SEQUENCE another; GRANT USAGE ON SEQUENCE another TO reader", &[]).unwrap();
    assert_eq!(
        restored.sql("SELECT id FROM items", &[]).unwrap().rows[0]["id"],
        Value::Int(1)
    );
    drop((restored, input, source, authority));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_sequence_input_retires_native_prefix_and_transaction_on_every_terminal_kind(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("sequences"));
    for index in 0..8 {
        source
            .sql(&format!("CREATE SEQUENCE sequence_{index}"), &[])
            .unwrap();
    }
    let catalog = source.storage.catalog.as_ref().unwrap();
    let before = catalog.load_sequence_rows().unwrap();
    let metadata = catalog.metadata_with_prefix("sequence_").unwrap();
    for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_sequences(&mut input);
        assert!(input.advance(10).unwrap().is_none());
        assert!(matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Sequences(_)
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
        let occupied = if terminal == "memory" {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        } else {
            None
        };
        match terminal {
            "cancel" => control.cancellation().cancel(),
            "unwind" => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("retire private sequence restoration");
                }));
                assert!(!backend.in_transaction());
                assert_eq!(control.memory().used(), 0);
                continue;
            }
            _ => (),
        }
        if terminal != "drop" {
            let mut failed = false;
            for _ in 0..65536 {
                match input.advance(usize::from(terminal != "zero")) {
                    Err(_) => {
                        failed = true;
                        break;
                    }
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("terminal sequence input completed"),
                }
            }
            assert!(failed && input.candidate.is_none());
            assert!(input.advance(1).is_err());
        }
        drop((input, occupied));
        assert!(!backend.in_transaction());
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(catalog.load_sequence_rows().unwrap(), before);
    assert_eq!(catalog.metadata_with_prefix("sequence_").unwrap(), metadata);
    source.sql("SELECT nextval('sequence_0')", &[]).unwrap();
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_sequence_input_rejects_invalid_recorded_oid_without_publishing_partial_catalog(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("sequences"));
    source.sql("CREATE SEQUENCE ids", &[]).unwrap();
    let catalog = source.storage.catalog.as_ref().unwrap();
    catalog
        .set_metadata("sequence_catalog_oid:invalid", "invalid")
        .unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_sequences(&mut input);
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
    let mut error = None;
    for _ in 0..65536 {
        match input.advance(1) {
            Ok(None) => (),
            Ok(Some(_)) => panic!("invalid OID catalog was accepted"),
            Err(value) => {
                error = Some(value);
                break;
            }
        }
    }
    assert_eq!(
        error.unwrap().to_string(),
        "sequence catalog OID entry `sequence_catalog_oid:invalid` holds invalid OID `invalid`"
    );
    assert!(input.candidate.is_none());
    assert!(!backend.in_transaction());
    assert_eq!(control.memory().used(), 0);
    catalog
        .delete_metadata("sequence_catalog_oid:invalid")
        .unwrap();
    source.sql("SELECT nextval('ids')", &[]).unwrap();
}
