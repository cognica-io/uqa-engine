//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated input becomes a native mutable owner without losing its admitted roots.

use super::{encode, fixture};
use crate::{
    inverted_index::{
        AnalyzerBindings, AnalyzerPhase, MemoryInvertedIndex, RetainedInvertedIndexBuilder,
    },
    memory_capture::MemoryCaptureRegistry,
    read_control::StorageReadControl,
    InvertedIndex, StorageBackendError,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::memory::MemoryError;

fn imported(control: &StorageReadControl) -> RetainedInvertedIndexBuilder {
    let bindings = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    let mut builder = RetainedInvertedIndexBuilder::new(&bindings, control).unwrap();
    builder
        .add_image_document(37, &mut fixture(false).as_slice())
        .unwrap();
    builder
}

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("x".into(), text.into())])
}

#[test]
fn writable_text_images_preserve_deferred_resolution_and_native_mutation_failures() {
    let config = uqa_analysis::Analyzer::new(
        uqa_analysis::Tokenizer::Pattern {
            pattern: "[".into(),
        },
        Vec::new(),
        Vec::new(),
    );
    let control = StorageReadControl::with_limit(1 << 20);
    let mut builder =
        RetainedInvertedIndexBuilder::new(&AnalyzerBindings::new(config), &control).unwrap();
    builder
        .add_image_document(37, &mut fixture(false).as_slice())
        .unwrap();
    let mut index = builder.finish_writable().unwrap();
    assert_eq!(encode(&index, 37), Some(fixture(false)));
    assert!(index.index_analyzer_revision("x").is_err());
    assert!(index.add_document(38, fields("not published")).is_err());
    assert_eq!(index.doc_count().unwrap(), 1);
    assert_eq!(encode(&index, 37), Some(fixture(false)));
    assert!(encode(&index, 38).is_none());
    index.remove_document(37).unwrap();
    index
        .set_field_analyzer(
            "x",
            uqa_analysis::whitespace_analyzer(),
            AnalyzerPhase::Both,
        )
        .unwrap();
    index.add_document(38, fields("new new")).unwrap();
    assert_eq!(index.get_term_freq(38, "x", "new").unwrap(), 2);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn writable_text_images_keep_exact_revisions_and_native_binding_transactions() {
    let indexed = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let searched = uqa_analysis::keyword_analyzer().compile().unwrap();
    let mut bindings = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    bindings
        .bind_revisions("x", indexed.clone(), searched.clone())
        .unwrap();
    let mut source = MemoryInvertedIndex::with_bindings(bindings.clone());
    source.add_document(1, fields("a β a")).unwrap();
    let bytes = encode(&source, 1).unwrap();
    drop(source);
    let control = StorageReadControl::with_limit(1 << 20);
    let mut builder = RetainedInvertedIndexBuilder::new(&bindings, &control).unwrap();
    builder
        .add_image_document(1, &mut bytes.as_slice())
        .unwrap();
    let mut index = builder.finish_writable().unwrap();
    drop(bindings);
    assert!(Arc::ptr_eq(
        &index.index_analyzer_revision("x").unwrap(),
        &indexed
    ));
    assert!(Arc::ptr_eq(
        &index.search_analyzer_revision("x").unwrap(),
        &searched
    ));
    assert!(index
        .set_field_analyzer_revision("x", searched.clone(), AnalyzerPhase::Index)
        .is_err());
    assert!(Arc::ptr_eq(
        &index.index_analyzer_revision("x").unwrap(),
        &indexed
    ));
    index
        .set_field_analyzer_revision("x", indexed.clone(), AnalyzerPhase::Search)
        .unwrap();
    assert!(Arc::ptr_eq(
        &index.search_analyzer_revision("x").unwrap(),
        &indexed
    ));
    assert_eq!(encode(&index, 1), Some(bytes));
    index.add_document(2, fields("β new")).unwrap();
    assert_eq!(index.doc_freq("x", "β").unwrap(), 2);
    index.clear().unwrap();
    index.remove_field_analyzers("x").unwrap();
    index.add_document(3, fields("ordinary default")).unwrap();
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn writable_text_corpus_reservations_follow_shared_roots_and_detach_on_write() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = imported(&control).finish_writable().unwrap();
    control.cancellation().cancel();
    let read = index.snapshot().unwrap();
    let mut fork = index.writable_snapshot().unwrap();
    let copied = index.clone();
    index.clear().unwrap();
    drop(index);
    assert!(control.memory().used() > fixture(false).len());
    assert_eq!(encode(&*read, 37), Some(fixture(false)));
    fork.add_document(38, fields("detached payload")).unwrap();
    assert_eq!(read.doc_count().unwrap(), 1);
    assert_eq!(fork.doc_count().unwrap(), 2);
    drop(read);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(encode(&copied, 37), Some(fixture(false)));
    assert_eq!(encode(&*fork, 37), Some(fixture(false)));
}

#[test]
fn writable_text_images_enter_live_capture_and_survive_unread_source_retirement() {
    let input = StorageReadControl::with_limit(1 << 20);
    let index = imported(&input).finish_writable().unwrap();
    let registry = MemoryCaptureRegistry::new();
    let source = registry
        .cell(Box::new(index) as Box<dyn InvertedIndex>)
        .unwrap();
    let key = source.key().unwrap();
    let retention = StorageReadControl::with_limit(1 << 20);
    let capture = registry.capture(&retention).unwrap();
    drop(source);
    let selected = key.captured(&capture).unwrap();
    drop((key, capture, registry));
    assert_eq!(encode(selected.as_ref().as_ref(), 37), Some(fixture(false)));
    assert_eq!(selected.total_field_length("x").unwrap(), 2);
    assert!(selected.writable_snapshot().is_err());
    assert!(input.memory().used() > 0);
    drop(selected);
    assert_eq!(input.memory().used(), 0);
    assert_eq!(retention.memory().used(), 0);
}

#[test]
fn writable_text_publication_rejection_and_unwinding_release_original_admission() {
    for available in [0, size_of::<MemoryInvertedIndex>() - 1] {
        let control = StorageReadControl::with_limit(1 << 20);
        let builder = imported(&control);
        let blocker = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used() - available)
            .unwrap();
        assert!(matches!(
            builder.finish_writable(),
            Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(control.memory().used(), blocker.bytes());
        drop(blocker);
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let builder = imported(&control);
    control.cancellation().cancel();
    assert!(builder.finish_writable().is_err());
    assert_eq!(control.memory().used(), 0);
    let control = StorageReadControl::with_limit(1 << 20);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _owner = imported(&control).finish_writable().unwrap();
        panic!("discard an unpublished writable corpus");
    }));
    assert!(unwind.is_err());
    assert_eq!(control.memory().used(), 0);
}
