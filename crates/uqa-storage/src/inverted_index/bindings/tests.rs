//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{read_control::StorageReadControl, StorageBackendError};

#[test]
fn frozen_field_containers_keep_admission_and_revisions_until_the_last_reader() {
    let index = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let search = uqa_analysis::keyword_analyzer().compile().unwrap();
    let name = "field_".repeat(8192);
    let mut live = AnalyzerBindings::new(uqa_analysis::standard_analyzer("english"));
    live.bind_revisions(&name, Arc::clone(&index), Arc::clone(&search))
        .unwrap();
    let rejected = StorageReadControl::with_limit(name.len() - 1);
    assert!(matches!(
        live.retained(&rejected),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(rejected.memory().used(), 0);
    assert!(Arc::ptr_eq(&live.index_revision(&name).unwrap(), &index));

    let control = StorageReadControl::with_limit(1 << 20);
    let frozen = live.retained(&control).unwrap();
    let bytes = control.memory().used();
    assert!(bytes > name.len());
    let nested = frozen.retained(&StorageReadControl::with_limit(0)).unwrap();
    assert_eq!(control.memory().used(), bytes);
    let mut writable = frozen.clone();
    writable.remove(&name);
    writable
        .bind_revision("new", Arc::clone(&search), AnalyzerPhase::Both)
        .unwrap();
    live.remove(&name);
    drop(live);
    drop(frozen);
    assert_eq!(control.memory().used(), bytes);
    assert!(Arc::ptr_eq(&nested.index_revision(&name).unwrap(), &index));
    assert!(Arc::ptr_eq(
        &nested.search_revision(&name).unwrap(),
        &search
    ));
    assert!(!Arc::ptr_eq(
        &writable.search_revision(&name).unwrap(),
        &search
    ));
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_or_cancelled_field_capture_releases_partial_metadata() {
    let revision = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let mut live = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    live.bind_revision("a", Arc::clone(&revision), AnalyzerPhase::Both)
        .unwrap();
    live.bind_revision(&"z".repeat(1 << 16), revision, AnalyzerPhase::Both)
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    assert!(matches!(
        live.retained(&control),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(control.memory().peak() > 0);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        live.retained(&control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn controlled_default_configuration_shares_its_catalog_owner() {
    let live = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    let revision = live.index_revision("unbound").unwrap();
    let control = StorageReadControl::with_limit(4096);
    let frozen =
        AnalyzerBindings::from_retained_revisions(live.default_binding(), [], &control).unwrap();
    assert!(std::ptr::eq(
        frozen.default_configuration(),
        live.default_configuration()
    ));
    let nested = frozen.retained(&StorageReadControl::with_limit(0)).unwrap();
    drop(live);
    drop(frozen);
    assert!(Arc::ptr_eq(
        &nested.index_revision("other").unwrap(),
        &revision
    ));
    assert!(Arc::ptr_eq(
        &nested.search_revision("other").unwrap(),
        &revision
    ));
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn public_retained_bindings_share_admitted_containers_and_only_lend_immutable_access() {
    let mut live = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    let revision = uqa_analysis::keyword_analyzer().compile().unwrap();
    live.bind_revision("body", Arc::clone(&revision), AnalyzerPhase::Both)
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    let frozen = RetainedAnalyzerBindings::capture(&live, &control).unwrap();
    let bytes = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - bytes)
        .unwrap();
    let nested = frozen.clone();
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(full);
    let mut writable = (*frozen).clone();
    writable.remove("body");
    drop((live, frozen));
    assert!(Arc::ptr_eq(
        &nested.index_revision("body").unwrap(),
        &revision
    ));
    assert_eq!(control.memory().used(), bytes);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}
