//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{
    mvcc::VersionedSessionOptions, read_control::StorageReadControl, StorageBackendError,
};

#[test]
fn unbound_snapshots_preserve_replacement_deletion_and_nested_lifetimes() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    index
        .add_document(1, fields([("body", "alpha alpha")]))
        .unwrap();
    index
        .add_document(2, fields([("body", "alpha beta")]))
        .unwrap();
    let revision = index.index_analyzer_revision("body").unwrap();
    let before = index
        .get_occurrence_postings("body", &TokenTermKey::from_text("alpha"))
        .unwrap();
    let captured = index.snapshot().unwrap();
    index.add_document(1, fields([("body", "gamma")])).unwrap();
    index.remove_document(2).unwrap();
    assert_eq!(index.doc_freq("body", "alpha").unwrap(), 0);
    assert_eq!(captured.doc_freq("body", "alpha").unwrap(), 2);
    let nested = captured.snapshot().unwrap();
    drop((captured, index));
    assert_eq!(
        nested
            .get_occurrence_postings("body", &TokenTermKey::from_text("alpha"))
            .unwrap(),
        before
    );
    assert_eq!(nested.get_doc_length(1, "body").unwrap(), 2);
    assert_eq!(nested.doc_count().unwrap(), 2);
    assert!(Arc::ptr_eq(
        &nested.index_analyzer_revision("body").unwrap(),
        &revision
    ));
}

#[test]
fn native_capture_admits_binding_names_and_releases_them_with_the_last_reader() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    let field = "native_field_".repeat(8192);
    let revision = uqa_analysis::keyword_analyzer().compile().unwrap();
    index
        .set_field_analyzer_revision(&field, Arc::clone(&revision), AnalyzerPhase::Both)
        .unwrap();
    index
        .conn
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let control = index.conn.retention_control().unwrap();
    let before = control.memory().used();
    let snapshot = index.snapshot().unwrap();
    let bytes = control.memory().used();
    assert!(bytes > before + field.len());
    let full = control
        .memory()
        .reserve(control.memory().limit() - bytes)
        .unwrap();
    assert!(matches!(
        index.snapshot(),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(full);
    let nested = snapshot
        .snapshot_with_control(&StorageReadControl::with_limit(0))
        .unwrap();
    assert_eq!(control.memory().used(), bytes);
    index.remove_field_analyzers(&field).unwrap();
    drop((snapshot, index));
    assert!(control.memory().used() > field.len());
    assert!(Arc::ptr_eq(
        &nested.index_analyzer_revision(&field).unwrap(),
        &revision
    ));
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn physical_capture_admits_binding_names_and_preserves_the_original_allowance() {
    let mut index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    let field = "physical_field_".repeat(8192);
    let revision = uqa_analysis::keyword_analyzer().compile().unwrap();
    index
        .set_field_analyzer_revision(&field, Arc::clone(&revision), AnalyzerPhase::Both)
        .unwrap();
    let rejected = StorageReadControl::with_limit(4096);
    assert!(matches!(
        index.snapshot_with_control(&rejected),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(rejected.memory().used(), 0);
    let control = StorageReadControl::with_limit(1 << 20);
    let snapshot = index.snapshot_with_control(&control).unwrap();
    let bytes = control.memory().used();
    assert!(bytes > field.len());
    let full = control
        .memory()
        .reserve(control.memory().limit() - bytes)
        .unwrap();
    assert!(matches!(
        index.snapshot_with_control(&control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(full);
    let nested = snapshot
        .snapshot_with_control(&StorageReadControl::with_limit(0))
        .unwrap();
    assert_eq!(control.memory().used(), bytes);
    index.remove_field_analyzers(&field).unwrap();
    drop((snapshot, index));
    assert!(Arc::ptr_eq(
        &nested.index_analyzer_revision(&field).unwrap(),
        &revision
    ));
    assert_eq!(control.memory().used(), bytes);
    control.cancellation().cancel();
    assert!(matches!(
        nested.index_analyzer_revision(&field),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn physical_default_capture_uses_its_shared_provider_allowance() {
    let index = idx_with_analyzer(uqa_analysis::whitespace_analyzer());
    let control = index.retention_control.clone();
    let snapshot = index.snapshot().unwrap();
    let bytes = control.memory().used();
    assert!(bytes > 0);
    let full = control
        .memory()
        .reserve(control.memory().limit() - bytes)
        .unwrap();
    assert!(matches!(
        index.snapshot(),
        Err(StorageBackendError::Memory(_))
    ));
    drop(full);
    drop(index);
    assert_eq!(control.memory().used(), bytes);
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}
