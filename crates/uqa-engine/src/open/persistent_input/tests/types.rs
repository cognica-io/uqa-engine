//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::fmt::Write;

mod domains;
mod structured;

fn assert_marker_error(error: StorageBackendError, text: &str) {
    let expected =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text).unwrap_err();
    let StorageBackendError::Json(actual) = error else {
        panic!("native type marker diagnostic: {error}");
    };
    assert_eq!(actual.to_string(), expected.to_string());
    assert_eq!(actual.is_data(), expected.is_data());
    assert_eq!(actual.is_syntax(), expected.is_syntax());
    assert_eq!(actual.is_eof(), expected.is_eof());
}

#[derive(Clone, Copy)]
enum Kind {
    Enum,
    Shell,
    Range,
    Comment,
    Composite,
    Domain,
}

fn selected(input: &PersistentEngineInput, kind: Kind) -> bool {
    matches!(
        (&input.candidate.as_ref().unwrap().stage, kind),
        (Stage::Types(PersistentTypeInput::Enums(_)), Kind::Enum)
            | (Stage::Types(PersistentTypeInput::Shells(_)), Kind::Shell)
            | (Stage::Types(PersistentTypeInput::Ranges(_)), Kind::Range)
            | (
                Stage::Types(PersistentTypeInput::Comments(_)),
                Kind::Comment
            )
            | (
                Stage::Types(PersistentTypeInput::Composites(_)),
                Kind::Composite
            )
            | (Stage::Types(PersistentTypeInput::Domains(_)), Kind::Domain)
    )
}
fn reach_types(input: &mut PersistentEngineInput, kind: Kind) {
    for _ in 0..65536 {
        if selected(input, kind) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("type catalog input was not reached")
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_type_input_keeps_selected_roots_and_later_sql_mutation_ownership(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("types"));
    source.sql("CREATE ROLE reader; CREATE TYPE mood AS ENUM('a','b'); CREATE TYPE pending; GRANT USAGE ON TYPE mood TO reader; CREATE TABLE items(v mood); INSERT INTO items VALUES('a'); GRANT SELECT ON items TO reader", &[]).unwrap();
    let enums = serde_json::to_value(&*source.durable.enums.read()).unwrap();
    let shells = serde_json::to_value(&*source.durable.shells.read()).unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_types(&mut input, Kind::Enum);
    assert!(input.advance(16).unwrap().is_none());
    source
        .sql(
            "ALTER TYPE mood RENAME VALUE 'a' TO 'selected_later'; CREATE TYPE later AS ENUM('z')",
            &[],
        )
        .unwrap();
    let session = finish(&mut input);
    assert_eq!(
        serde_json::to_value(&*session.engine().durable.enums.read()).unwrap(),
        enums
    );
    assert_eq!(
        serde_json::to_value(&*session.engine().durable.shells.read()).unwrap(),
        shells
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
            .sql("SELECT v::text AS v FROM items", &[])
            .unwrap()
            .rows[0]["v"],
        Value::Str("selected_later".into())
    );
    restored.sql("RESET ROLE; ALTER TYPE mood ADD VALUE 'c'; DROP TYPE pending; CREATE TYPE another; CREATE TYPE completed AS ENUM('x')", &[]).unwrap();
    assert!(restored.durable.enums.read()["public.mood"]
        .labels
        .iter()
        .any(|label| label.label == "c"));
    assert!(restored.durable.enums.read().contains_key("public.later"));
    assert!(restored
        .durable
        .shells
        .read()
        .contains_key("public.another"));
    assert!(!restored
        .durable
        .shells
        .read()
        .contains_key("public.pending"));
    drop((restored, input, source));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

fn terminate(source: &Engine, kind: Kind, terminal: &str) {
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(source, &control);
    reach_types(&mut input, kind);
    assert!(input.advance(64).unwrap().is_none());
    assert!(selected(&input, kind));
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
                panic!("retire private type restoration");
            }));
            assert!(!backend.in_transaction());
            assert_eq!(control.memory().used(), 0);
            return;
        }
        _ => (),
    }
    if terminal != "drop" {
        loop {
            match input.advance(usize::from(terminal != "zero")) {
                Err(_) => break,
                Ok(None) => (),
                Ok(Some(_)) => panic!("terminal type restoration completed"),
            }
        }
        assert!(input.candidate.is_none());
        assert!(input.advance(1).is_err());
    }
    drop((input, occupied));
    assert!(!backend.in_transaction());
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_type_input_retires_both_private_roots_and_the_selected_transaction(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("types"));
    let mut statements = String::new();
    for i in 0..16 {
        write!(
            statements,
            "CREATE TYPE e_{i} AS ENUM('a','b','c'); CREATE TYPE s_{i};"
        )
        .unwrap();
    }
    source.sql(&statements, &[]).unwrap();
    let catalog = source.storage.catalog.as_ref().unwrap();
    let original = catalog.metadata_with_prefix("uqa.sql.").unwrap();
    for kind in [Kind::Enum, Kind::Shell] {
        for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
            terminate(&source, kind, terminal);
        }
    }
    assert_eq!(catalog.metadata_with_prefix("uqa.sql.").unwrap(), original);
    for marker in ["sql_enums_json", "sql_shell_types_json"] {
        let saved = catalog.get_metadata(marker).unwrap().unwrap();
        catalog.set_metadata(marker, "{bad marker JSON").unwrap();
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
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
        loop {
            match input.advance(64) {
                Err(error) => {
                    assert_marker_error(error, "{bad marker JSON");
                    break;
                }
                Ok(None) => (),
                Ok(Some(_)) => panic!("corrupt type marker published"),
            }
        }
        assert!(!backend.in_transaction());
        assert!(input.advance(1).is_err());
        assert_eq!(control.memory().used(), 0);
        catalog.set_metadata(marker, &saved).unwrap();
    }
    source
        .sql("CREATE TYPE recovered AS ENUM('ok')", &[])
        .unwrap();
}
