//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn metadata(name: &str) -> CursorMetadata {
    CursorMetadata {
        name: name.into(),
        source_sql: Some("SELECT 1".into()),
        is_holdable: false,
        is_binary: false,
        is_scrollable: false,
        created_at_micros: 42,
    }
}

#[test]
fn detached_executor_retains_visibility_and_exclusive_name_until_drop() {
    let registry = PortalRegistry::default();
    let registration = registry.register(metadata("cursor")).unwrap();
    let mut executors = BTreeMap::from([("cursor", registration)]);
    let detached = executors.remove("cursor").unwrap();
    assert!(executors.is_empty());
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].name, "cursor");
    assert_eq!(snapshot[0].created_at_micros, 42);
    assert_eq!(
        registry.ensure_available("cursor").unwrap_err().sqlstate(),
        Some("42P03")
    );
    assert!(matches!(
        registry.register(metadata("cursor")),
        Err(error) if error.sqlstate() == Some("42P03")
    ));
    drop(detached);
    assert!(registry.snapshot().is_empty());
    assert_eq!(snapshot[0].name, "cursor");
    let replacement = registry.register(metadata("cursor")).unwrap();
    drop(snapshot);
    assert_eq!(registry.snapshot().len(), 1);
    drop(replacement);
    assert!(registry.snapshot().is_empty());
}

#[test]
fn generated_names_skip_live_explicit_names_and_are_session_local() {
    let registry = PortalRegistry::default();
    let explicit = registry.register(metadata("<unnamed portal 1>")).unwrap();
    assert_eq!(registry.allocate_name(), "<unnamed portal 2>");
    let independent = PortalRegistry::default();
    assert_eq!(independent.allocate_name(), "<unnamed portal 1>");
    assert!(independent.snapshot().is_empty());
    drop(explicit);
    assert_eq!(registry.allocate_name(), "<unnamed portal 3>");
}

#[test]
fn dropping_the_session_before_its_executor_does_not_retain_registry_state() {
    let registry = PortalRegistry::default();
    let registration = registry.register(metadata("cursor")).unwrap();
    let state = Arc::downgrade(&registry.state);
    drop(registry);
    assert!(state.upgrade().is_none());
    drop(registration);
}
