//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained subscriptions surface registry replacement without adopting its state.

use super::*;

#[rstest::rstest]
fn replacing_the_registry_terminates_original_listeners(#[values(false, true)] encrypted: bool) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("original.db");
    let replacement = directory.path().join("replacement.db");
    let open = |path: &std::path::Path| {
        if encrypted {
            Engine::open_encrypted(path, super::super::encryption::KEY).unwrap()
        } else {
            Engine::open(path).unwrap()
        }
    };
    let engine = open(&path);
    let first = engine
        .subscribe_notifications(&["private_channel"], options())
        .unwrap();
    let second = engine
        .subscribe_notifications(&["private_channel"], options())
        .unwrap();
    let other = open(&replacement);
    other
        .subscribe_notifications(&["other_channel"], options())
        .unwrap()
        .close();
    drop(other);
    let registry = super::super::encryption::registry_path(&path);
    let foreign = super::super::encryption::registry_path(&replacement);
    let expected = std::fs::read(&foreign).unwrap();
    std::fs::rename(&foreign, &registry).unwrap();
    // The actual retained recovery worker must report the failure without a SQL
    // call triggering delivery or any manual reconnection by the application.
    for subscription in [&first, &second] {
        let error = subscription.wait(Duration::from_secs(5)).unwrap_err();
        assert_eq!(error.kind(), NotificationFailureKind::SourceUnavailable);
        let diagnostic = format!("{error:?} {error}");
        for private in ["private_channel", "original.db", "replacement.db"] {
            assert!(!diagnostic.contains(private), "{diagnostic}");
        }
        subscription.close();
        assert_eq!(
            subscription.poll().unwrap_err().kind(),
            NotificationFailureKind::SourceUnavailable
        );
    }
    assert!(engine
        .subscribe_notifications(&["private_channel"], options())
        .is_err());
    drop(engine);
    assert_eq!(std::fs::read(registry).unwrap(), expected);
}
