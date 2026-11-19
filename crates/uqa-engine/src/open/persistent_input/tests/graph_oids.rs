//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_graph::LabelKind;

fn reach_oids(input: &mut PersistentEngineInput) {
    for _ in 0..65536 {
        if matches!(input.candidate.as_ref().unwrap().stage, Stage::GraphOids(_)) {
            return;
        }
        assert!(input.advance(1).unwrap().is_none());
    }
    panic!("graph OID input was not reached")
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_graph_oid_input_keeps_selected_identity_roots_and_live_later_mutations(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("graph-oids"));
    source.create_graph("g").unwrap();
    source
        .create_graph_label("g", "node", LabelKind::Vertex)
        .unwrap();
    source
        .create_graph_label("g", "edge", LabelKind::Edge)
        .unwrap();
    let expected = source.durable.graph_catalog_oids.read().clone();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_oids(&mut input);
    for _ in 0..16 {
        assert!(input.advance(1).unwrap().is_none());
    }
    source.create_graph("later").unwrap();
    let session = finish(&mut input);
    assert_eq!(
        *session.engine().durable.graph_catalog_oids.read(),
        expected
    );
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.create_graph("after").unwrap();
    restored
        .create_graph_label("after", "node", LabelKind::Vertex)
        .unwrap();
    let current = restored.durable.graph_catalog_oids.read().clone();
    assert_eq!(current.get("g"), expected.get("g"));
    assert!(current.contains_key("later") && current.contains_key("after"));
    drop((current, restored, input, source));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_graph_oid_input_retires_private_identity_roots_and_selected_transactions(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("graph-oids"));
    source.create_graph("g").unwrap();
    for index in 0..16 {
        source
            .create_graph_label("g", &format!("node_{index}"), LabelKind::Vertex)
            .unwrap();
    }
    let catalog = source.storage.catalog.as_ref().unwrap();
    let before = catalog.metadata_with_prefix("graph_catalog_oids:").unwrap();
    for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&source, &control);
        reach_oids(&mut input);
        for _ in 0..64 {
            assert!(input.advance(1).unwrap().is_none());
        }
        assert!(matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::GraphOids(_)
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
                    let _owner = input;
                    panic!("retire private graph OID restoration");
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
                    Ok(Some(_)) => panic!("terminal graph OID restoration completed"),
                }
            }
            assert!(input.candidate.is_none());
            assert!(input.advance(1).is_err());
        }
        drop((input, occupied));
        assert!(!backend.in_transaction());
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(
        catalog.metadata_with_prefix("graph_catalog_oids:").unwrap(),
        before
    );
    catalog
        .set_metadata("graph_catalog_oids:g", "{bad JSON")
        .unwrap();
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
                assert!(error
                    .to_string()
                    .contains("invalid stored graph catalog OIDs"));
                break;
            }
            Ok(None) => (),
            Ok(Some(_)) => panic!("corrupt OIDs published"),
        }
    }
    assert!(!backend.in_transaction());
    assert_eq!(control.memory().used(), 0);
    for (key, value) in before {
        catalog.set_metadata(&key, &value).unwrap();
    }
    source.create_graph("recovered").unwrap();
}
