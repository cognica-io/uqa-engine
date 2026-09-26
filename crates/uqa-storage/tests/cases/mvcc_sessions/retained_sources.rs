//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::key_value::KeyValueRead;

fn capture(store: &dyn KeyValueStore) -> Arc<dyn KeyValueRead + Send + Sync> {
    let mut source = None;
    store
        .with_read_view(&mut |read| {
            source = Some(read.retain(&[b""])?);
            Ok(())
        })
        .unwrap();
    source.unwrap()
}

#[test]
fn private_metadata_sources_follow_replacement_and_release_after_last_view() {
    for removal in 0..3 {
        let persistence = Persistence::new();
        let store = persistence.session(1 << 20);
        store.put(b"source", b"original").unwrap();
        let source = capture(&store);
        let weak = Arc::downgrade(&source);
        store.begin_transaction().unwrap();
        store
            .with_mutation(&mut |_, batch| {
                batch.put_with_retained_source(b"head", b"selection", source.clone())
            })
            .unwrap();
        let held = capture(&store);
        drop(source);
        match removal {
            0 => store.put(b"head", b"selection").unwrap(),
            1 => store.delete(b"head").unwrap(),
            _ => {
                store.delete_prefix(b"he").unwrap();
            }
        }
        assert!(capture(&store).retained_source(b"head").unwrap().is_none());
        assert_eq!(
            held.retained_source(b"head")
                .unwrap()
                .unwrap()
                .get(b"source")
                .unwrap()
                .as_deref(),
            Some(&b"original"[..])
        );
        assert!(weak.upgrade().is_some());
        drop(held);
        assert!(
            weak.upgrade().is_none(),
            "obsolete live-view source must be released"
        );
        store.rollback_transaction().unwrap();
    }
}

#[test]
fn private_metadata_sources_are_atomic_with_later_failed_operations() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    store.put(b"stable", b"original").unwrap();
    let source = capture(&store);
    store.begin_transaction().unwrap();
    let result = store.with_mutation(&mut |_, batch| {
        batch.put_with_retained_source(b"head", b"selection", source.clone())?;
        batch.put(b"stable", b"discarded")?;
        // Requiring an independently observed value for an already private key must reject the entire evaluated batch.
        batch.require_observed(b"stable", &source.record_revision(b"stable")?.unwrap())
    });
    assert!(result.is_err());
    assert_eq!(
        store.get(b"stable").unwrap().as_deref(),
        Some(&b"original"[..])
    );
    assert!(store.get(b"head").unwrap().is_none());
    assert!(capture(&store).retained_source(b"head").unwrap().is_none());
    store.rollback_transaction().unwrap();
}

#[test]
fn private_metadata_sources_preserve_undo_and_do_not_persist_in_committed_rows() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    store.put(b"source", b"first").unwrap();
    let first = capture(&store);
    store.put(b"source", b"second").unwrap();
    let second = capture(&store);
    store.begin_transaction().unwrap();
    store
        .with_mutation(&mut |_, batch| {
            batch.put_with_retained_source(b"head", b"first", first.clone())
        })
        .unwrap();
    store.savepoint("first_selection").unwrap();
    store
        .with_mutation(&mut |_, batch| {
            batch.put_with_retained_source(b"head", b"second", second.clone())
        })
        .unwrap();
    let private = capture(&store);
    store.rollback_to_savepoint("first_selection").unwrap();
    let restored = capture(&store).retained_source(b"head").unwrap().unwrap();
    assert_eq!(
        restored.get(b"source").unwrap().as_deref(),
        Some(&b"first"[..])
    );
    assert_eq!(
        private
            .retained_source(b"head")
            .unwrap()
            .unwrap()
            .get(b"source")
            .unwrap()
            .as_deref(),
        Some(&b"second"[..])
    );
    store.commit_transaction().unwrap();
    assert_eq!(store.get(b"head").unwrap().as_deref(), Some(&b"first"[..]));
    assert!(capture(&store).retained_source(b"head").unwrap().is_none());
    assert!(private.retained_source(b"head").unwrap().is_some());
}
