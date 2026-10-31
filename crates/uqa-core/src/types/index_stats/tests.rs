//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, ValueRetentionError,
};

fn physical() -> DiskANNQueryStats {
    DiskANNQueryStats {
        index: DiskANNIndexStats {
            generation: VectorGeneration {
                database: [1; 16],
                table: 2,
                index: 3,
                generation: 4,
            },
            dimensions: 2,
            max_degree: 8,
            search_list_size: 12,
            beam_width: 2,
            pq_bytes: 4,
            pq_centroids: Some(16),
            node_slot_bytes: 64,
            node_fragments: 1,
            graph_pages: 10,
            graph_edges: Some(30),
            page_bytes: 4096,
            populations: VectorPopulationStats {
                base_documents: Some(10),
                base_vectors: 10,
                graph_nodes: 10,
                side_vectors: 10,
                current_vectors: Some(10),
                changed_vectors: Some(0),
            },
            reads: VectorReadStats {
                resident_bytes: 16,
                cache_bytes: 4096,
                max_record_bytes: 4096,
                max_in_flight_page_bytes: 4096,
                max_batch_pages: 1,
                read_concurrency: 1,
            },
        },
        query_route: VectorQueryRoute::Approximate,
    }
}

#[test]
fn frequency_lookups_borrow_large_scalar_and_raw_utf16_keys() {
    let field = "f".repeat(1024 * 1024);
    let scalar = "😀".repeat(256 * 1024);
    let units: Vec<_> = scalar.encode_utf16().collect();
    let raw = vec![0xd800; 512 * 1024];
    let mut stats = IndexStats::new(100);
    stats.set_doc_freq(field.clone(), scalar.clone(), 17);
    stats.set_doc_freq_utf16(field.clone(), raw.clone(), 29);
    let allocation = allocation_counter::measure(|| {
        assert_eq!(stats.doc_freq(&field, &scalar), 17);
        assert_eq!(stats.doc_freq_utf16(&field, &units), 17);
        assert_eq!(stats.doc_freq_utf16(&field, &raw), 29);
        assert_eq!(stats.doc_freq("missing", &scalar), 0);
        assert_eq!(stats.doc_freq_utf16("missing", &raw), 0);
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn admitted_statistics_retain_exact_allocations_and_lossless_key_identity() {
    let memory = MemoryBudget::new(8 * 1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let term = "x".repeat(1024 * 1024);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        let mut stats = IndexStatsBuilder::new(100, control);
        stats.set_doc_freq("body", &term, 11).unwrap();
        stats.set_doc_freq("body", "🙂", 17).unwrap();
        stats
            .set_doc_freq_utf16("body", &[0xd83d, 0xde42], 19)
            .unwrap();
        stats.set_doc_freq_utf16("body", &[0xd800], 23).unwrap();
        stats.set_doc_freq("body", "�", 29).unwrap();
        stats
            .set_diskann_query("v", &[0.0, 1.0], physical())
            .unwrap();
        let mut zero = physical();
        zero.query_route = VectorQueryRoute::ExactZeroNorm;
        stats.set_diskann_query("v", &[-0.0, 1.0], zero).unwrap();
        output = Some(stats.finish().unwrap());
    });
    let output = output.unwrap();
    assert_eq!(output.reserved_bytes(), memory.used());
    assert_eq!(
        usize::try_from(allocation.bytes_current).unwrap(),
        memory.used(),
        "{allocation:?}"
    );
    assert_eq!(output.doc_freq("body", &term), 11);
    assert_eq!(output.doc_freq("body", "🙂"), 19);
    assert_eq!(output.doc_freq_utf16("body", &[0xd800]), 23);
    assert_eq!(output.doc_freq_utf16("body", &[0xfffd]), 29);
    assert_eq!(
        output.diskann_query("v", &[0.0, 1.0]).unwrap().query_route,
        VectorQueryRoute::Approximate
    );
    assert_eq!(
        output.diskann_query("v", &[-0.0, 1.0]).unwrap().query_route,
        VectorQueryRoute::ExactZeroNorm
    );
    let before = memory.used();
    let output = output.with_total_docs(200);
    assert_eq!(output.total_docs, 200);
    assert_eq!(memory.used(), before);
    drop(output);
    assert_eq!(memory.used(), 0);
}

#[test]
fn statistics_replacements_need_no_capacity_and_failed_growth_preserves_the_owner() {
    let ancestor = MemoryBudget::new(16 * 1024);
    let child = ancestor.child(usize::MAX);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&child, &original, &invoking);
    let mut stats = IndexStatsBuilder::new(100, control);
    stats.set_doc_freq("body", "🙂", 1).unwrap();
    stats
        .set_diskann_query("v", &[1.0, -0.0], physical())
        .unwrap();
    let before = child.used();
    let sibling = ancestor.reserve(ancestor.available()).unwrap();
    let allocation = allocation_counter::measure(|| {
        stats
            .set_doc_freq_utf16("body", &[0xd83d, 0xde42], 2)
            .unwrap();
        stats
            .set_diskann_query("v", &[1.0, -0.0], physical())
            .unwrap();
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert!(matches!(
        stats.set_doc_freq("body", "new", 3),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(stats.doc_freq("body", "🙂"), 2);
    assert_eq!(stats.doc_freq("body", "new"), 0);
    assert_eq!(child.used(), before);
    assert_eq!(ancestor.used(), before + sibling.bytes());
    let output = stats.finish().unwrap();
    drop(output);
    assert_eq!(child.used(), 0);
    assert_eq!(ancestor.used(), sibling.bytes());
}

#[test]
fn statistics_admission_rejects_large_keys_before_copying_and_releases_partial_keys() {
    let term = "x".repeat(4 * 1024 * 1024);
    let query = vec![1.0; 1024 * 1024];
    let memory = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut stats = IndexStatsBuilder::new(1, control);
    stats.set_doc_freq("body", "existing", 1).unwrap();
    let before = memory.used();
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            stats.set_doc_freq("body", &term, 2),
            Err(ValueRetentionError::Memory(_))
        ));
        assert!(matches!(
            stats.set_diskann_query("embedding", &query, physical()),
            Err(ValueRetentionError::Memory(_))
        ));
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(allocation.bytes_current, 0, "{allocation:?}");
    assert_eq!(memory.used(), before);
    assert_eq!(stats.doc_freq("body", "existing"), 1);
    drop(stats);
    assert_eq!(memory.used(), 0);
}

#[test]
fn statistics_honor_both_cancellation_scopes_and_release_failed_publication() {
    for cancel_original in [true, false] {
        let memory = MemoryBudget::new(1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut stats = IndexStatsBuilder::new(1, control);
        stats.set_doc_freq("body", "existing", 1).unwrap();
        let before = memory.used();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            stats.set_doc_freq("body", "existing", 2),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(stats.doc_freq("body", "existing"), 1);
        assert_eq!(memory.used(), before);
        assert!(matches!(
            stats.finish(),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn statistics_release_builder_and_published_output_on_unwind() {
    for publish in [false, true] {
        let memory = MemoryBudget::new(1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut stats = IndexStatsBuilder::new(1, control);
            stats.set_doc_freq("body", "term", 1).unwrap();
            if publish {
                let _output = stats.finish().unwrap();
                std::panic::resume_unwind(Box::new(()));
            } else {
                let _builder = stats;
                std::panic::resume_unwind(Box::new(()));
            }
        }));
        assert!(outcome.is_err());
        assert_eq!(memory.used(), 0);
    }
}
