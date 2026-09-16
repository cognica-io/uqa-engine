//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn evaluated_memory_batches_discard_errors_unwinds_and_cancellation() {
    super::super::conformance::verify_compound_mutations(&MemoryKeyValueStore::new()).unwrap();
}

#[test]
fn hnsw_handles_follow_memory_undo_and_reject_canonical_drift() {
    super::super::conformance::verify_hnsw_undo(store()).unwrap();
}

#[test]
fn ivf_handles_follow_memory_undo_and_definition_changes() {
    super::super::conformance::verify_ivf_undo(store()).unwrap();
}

#[test]
fn vector_snapshots_retain_tensors_and_reject_mutation_after_live_handles_close() {
    super::super::conformance::verify_vector_snapshots(&store()).unwrap();
}

#[test]
fn occurrence_snapshots_retain_postings_counters_and_analyzer_metadata() {
    super::super::conformance::verify_occurrence_snapshots(&store()).unwrap();
}

#[test]
fn retained_memory_read_copies_only_selected_keys_and_releases_failed_capture() {
    let store = store();
    store.put(b"selected/a", &[1; 1024]).unwrap();
    store.put(b"selected/b", &[2; 1024]).unwrap();
    store.put(b"unrelated", &[3; 4096]).unwrap();
    let (read, control) = super::super::index_view::read_view(store.as_ref(), |read| {
        Ok((
            read.retain(&[b"selected/", b"selected/a"])?,
            read.control().clone(),
        ))
    })
    .unwrap();
    let retained = control.memory().used();
    assert!(retained > 2048 && retained < 8192);
    store.put(b"selected/a", b"changed").unwrap();
    store.delete(b"selected/b").unwrap();
    assert_eq!(
        read.get(b"selected/a").unwrap().as_deref(),
        Some([1; 1024].as_slice())
    );
    assert_eq!(
        read.get(b"selected/b").unwrap().as_deref(),
        Some([2; 1024].as_slice())
    );
    assert!(read.get(b"unrelated").unwrap().is_none());
    let mut count = 0;
    read.visit_prefix(b"selected/", &mut |_, _| {
        count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 2);
    let hold = control
        .memory()
        .reserve(control.memory().limit() - retained - 512)
        .unwrap();
    let occupied = control.memory().used();
    assert!(matches!(
        super::super::index_view::read_view(store.as_ref(), |read| read.retain(&[b"unrelated"])),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), occupied);
    drop(hold);
    drop(read);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn occurrence_frequency_visitors_run_after_releasing_the_store_read() {
    let store = store();
    let mut index =
        KeyValueInvertedIndex::new(store.clone(), "docs", uqa_analysis::whitespace_analyzer());
    index
        .add_document(1, BTreeMap::from([("body".into(), "alpha alpha".into())]))
        .unwrap();
    let mut other =
        KeyValueInvertedIndex::new(store.clone(), "docs", uqa_analysis::whitespace_analyzer());
    let mut seen = Vec::new();
    index
        .for_each_term_freq("body", "alpha", &mut |doc, frequency| {
            seen.push((doc, frequency));
            other
                .add_document(2, BTreeMap::from([("body".into(), "alpha".into())]))
                .unwrap();
        })
        .unwrap();
    assert_eq!(seen, vec![(1, 2)]);
    assert_eq!(index.doc_count().unwrap(), 2);
}
