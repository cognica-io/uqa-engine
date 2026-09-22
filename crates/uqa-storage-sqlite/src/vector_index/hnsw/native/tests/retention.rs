//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::read_control::StorageReadControl;

#[test]
fn native_graph_cache_replacement_retains_each_live_generations_original_allowance() {
    let (connection, mut index) = fixture();
    let control = connection.retention_control().unwrap();
    let old = graph(&index);
    let nested = old
        .snapshot_with_control(&StorageReadControl::with_limit(0))
        .unwrap();
    assert!(control.memory().used() > 0);
    index.add(3, vec![-1.0, 0.0]).unwrap();
    let current = graph(&index);
    assert!(!std::ptr::eq(&raw const *old, &raw const *current));
    assert_eq!(current.count().unwrap(), 3);
    assert_eq!(old.count().unwrap(), 2);
    drop((index, connection));
    let both = control.memory().used();
    drop(current);
    assert!(control.memory().used() < both);
    assert!(control.memory().used() > 0);
    drop(old);
    assert!(control.memory().used() > 0);
    assert_eq!(nearest(&*nested, &[1.0, 0.0]), vec![1]);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_native_cache_replacement_preserves_the_previous_retained_generation() {
    let (connection, index) = fixture();
    let control = connection.retention_control().unwrap();
    let old = graph(&index);
    let writer = connection.new_session();
    let mut other = SQLiteHNSWIndex::new(writer.clone(), "docs", "embedding", 2);
    other.add(3, vec![-1.0, 0.0]).unwrap();
    let used = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - used)
        .unwrap();
    assert!(index.snapshot().is_err());
    assert!(matches!(
        old.search_knn(&[1.0, 0.0], 1),
        Err(uqa_storage::StorageBackendError::Memory(_))
    ));
    assert_eq!(old.count().unwrap(), 2);
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(full);
    assert_eq!(control.memory().used(), used);
    assert_eq!(nearest(&old, &[1.0, 0.0]), vec![1]);
    let current = graph(&index);
    assert_eq!(current.count().unwrap(), 3);
    drop((other, writer, current, index, connection, old));
    assert_eq!(control.memory().used(), 0);
}
