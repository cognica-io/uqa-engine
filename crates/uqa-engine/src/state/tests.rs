//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{EpochCoordinator, SessionContext};

#[test]
fn session_state_readers_never_observe_a_torn_pair() {
    let session = Arc::new(SessionContext::new(crate::random_state_from_seed(1)));
    {
        let mut state = session.state.write();
        state.search_path = vec!["0".to_string()];
        state.session_vars.insert("generation".into(), "0".into());
    }
    let done = Arc::new(AtomicBool::new(false));
    let writer_session = Arc::clone(&session);
    let writer_done = Arc::clone(&done);
    let writer = std::thread::spawn(move || {
        for generation in 1..10_000 {
            let generation = generation.to_string();
            let mut state = writer_session.state.write();
            state.search_path = vec![generation.clone()];
            state.session_vars.insert("generation".into(), generation);
        }
        writer_done.store(true, Ordering::Release);
    });

    while !done.load(Ordering::Acquire) {
        let state = session.state.read();
        assert_eq!(
            state.search_path.first(),
            state.session_vars.get("generation")
        );
    }
    writer.join().unwrap();
}

#[test]
fn derived_epoch_coordinator_shares_only_publication() {
    let source = EpochCoordinator::new();
    source.table_data.published.store(7, Ordering::Release);
    source.table_data.seen.store(6, Ordering::Release);
    source.table_data.dirty.store(true, Ordering::Release);
    let observed = source.published_epochs();
    source.table_data.published.store(8, Ordering::Release);

    let mut derived = EpochCoordinator::new();
    derived.share_published_from_at(&source, observed);

    assert!(Arc::ptr_eq(
        &source.table_data.published,
        &derived.table_data.published
    ));
    assert_eq!(derived.table_data.seen.load(Ordering::Acquire), 7);
    assert_eq!(derived.table_data.published.load(Ordering::Acquire), 8);
    assert!(!derived.table_data.dirty.load(Ordering::Acquire));
    derived.table_data.dirty.store(true, Ordering::Release);
    assert!(source.table_data.dirty.load(Ordering::Acquire));
    derived.table_data.dirty.store(false, Ordering::Release);
    assert!(source.table_data.dirty.load(Ordering::Acquire));
}
