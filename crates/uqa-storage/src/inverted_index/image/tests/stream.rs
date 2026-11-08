//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;

#[test]
fn text_image_identity_seek_preserves_sparse_and_empty_term_documents_without_allocation() {
    let retention = StorageReadControl::with_limit(1 << 20);
    let mut imported = builder(&retention);
    let bytes = fixture(false);
    let mut empty = bytes[..117].to_vec();
    empty[65..73].copy_from_slice(&0_u64.to_le_bytes());
    empty[109..117].copy_from_slice(&0_u64.to_le_bytes());
    for (id, image) in [(0, &bytes), (7, &empty), (u64::MAX, &bytes)] {
        imported
            .add_image_document(id, &mut image.as_slice())
            .unwrap();
    }
    let source = imported.finish().unwrap();
    let control = StorageReadControl::with_limit(0);
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            source.next_image_document_id(None, &control).unwrap(),
            Some(0)
        );
        assert_eq!(
            source.next_image_document_id(Some(0), &control).unwrap(),
            Some(7)
        );
        assert_eq!(
            source.next_image_document_id(Some(6), &control).unwrap(),
            Some(7)
        );
        assert_eq!(
            source.next_image_document_id(Some(7), &control).unwrap(),
            Some(u64::MAX)
        );
        assert_eq!(
            source
                .next_image_document_id(Some(u64::MAX), &control)
                .unwrap(),
            None
        );
    });
    assert_eq!(allocation.count_total, 0);
    retention.cancellation().cancel();
    assert!(source.next_image_document_id(None, &control).is_err());
    drop(source);
    assert_eq!(retention.memory().used(), 0);
    let empty = MemoryInvertedIndex::new(uqa_analysis::keyword_analyzer());
    assert_eq!(empty.next_image_document_id(None, &control).unwrap(), None);
    control.cancellation().cancel();
    assert!(empty.next_image_document_id(None, &control).is_err());
}

fn compare(index: &dyn InvertedIndex, id: u64, expected: &[u8], window: usize) {
    let control = StorageReadControl::with_limit(16 << 10);
    let mut reader = index.image_document_reader(id, &control).unwrap().unwrap();
    assert!(control.memory().used() > 0);
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    let mut bytes = vec![0; 32 << 10];
    let mut offset = 0;
    loop {
        let count = reader.read(&mut bytes[..window]).unwrap();
        assert!(count <= window.min(16 << 10));
        if count == 0 {
            break;
        }
        assert_eq!(&bytes[..count], &expected[offset..offset + count]);
        offset += count;
    }
    assert_eq!(offset, expected.len());
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert_eq!(reader.read(&mut bytes).unwrap(), 0);
}

#[test]
fn text_image_reader_preserves_independent_wire_fixtures_across_read_windows() {
    for discount in [false, true] {
        let retention = StorageReadControl::with_limit(1 << 20);
        let bytes = fixture(discount);
        let mut imported = builder(&retention);
        imported
            .add_image_document(37, &mut bytes.as_slice())
            .unwrap();
        let source = imported.finish().unwrap();
        for window in [1, 7, 31, 4096, 32 << 10] {
            compare(&source, 37, &bytes, window);
        }
        let missing = StorageReadControl::with_limit(0);
        assert!(source
            .image_document_reader(99, &missing)
            .unwrap()
            .is_none());
        drop(source);
        assert_eq!(retention.memory().used(), 0);
    }
    let mut source = MemoryInvertedIndex::new(uqa_analysis::keyword_analyzer());
    source.add_document(1, BTreeMap::new()).unwrap();
    let control = StorageReadControl::with_limit(16 << 10);
    assert!(source.image_document_reader(1, &control).unwrap().is_none());
    let mut empty = fixture(false)[..117].to_vec();
    empty[65..73].copy_from_slice(&0_u64.to_le_bytes());
    empty[109..117].copy_from_slice(&0_u64.to_le_bytes());
    let retention = StorageReadControl::with_limit(1 << 20);
    let mut imported = builder(&retention);
    imported
        .add_image_document(1, &mut empty.as_slice())
        .unwrap();
    compare(&imported.finish().unwrap(), 1, &empty, 1);
}

#[test]
fn text_image_reader_streams_large_raw_terms_without_payload_scratch() {
    let original = fixture(false);
    let mut bytes = original[..117].to_vec();
    let length = (2 << 20) + 1;
    bytes.extend_from_slice(&(length as u64).to_le_bytes());
    bytes.push(1);
    for _ in 0..(1 << 20) {
        bytes.extend_from_slice(&[0xd8, 0x00]);
    }
    bytes.extend_from_slice(&original[128..]);
    let retention = StorageReadControl::with_limit(64 << 20);
    let mut imported = builder(&retention);
    imported
        .add_image_document(37, &mut bytes.as_slice())
        .unwrap();
    let source = imported.finish().unwrap();
    let control = StorageReadControl::with_limit(16 << 10);
    let measured = allocation_counter::measure(|| {
        let mut reader = source.image_document_reader(37, &control).unwrap().unwrap();
        let mut output = [0; 4093];
        let mut offset = 0;
        loop {
            let count = reader.read(&mut output).unwrap();
            if count == 0 {
                break;
            }
            assert_eq!(&output[..count], &bytes[offset..offset + count]);
            offset += count;
        }
        assert_eq!(offset, bytes.len());
    });
    assert!(measured.bytes_total < 16 << 10, "{measured:?}");
    assert_eq!(control.memory().used(), 0);
    assert!(retention.memory().used() > 0);
    drop(source);
    assert_eq!(retention.memory().used(), 0);
}

#[test]
fn text_image_reader_admission_and_each_cancellation_owner_are_terminal() {
    let mut source = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    source.add_document(1, fields("a β a")).unwrap();
    let probe = StorageReadControl::with_limit(16 << 10);
    let reader = source.image_document_reader(1, &probe).unwrap().unwrap();
    let required = probe.memory().used();
    drop(reader);
    assert_eq!(probe.memory().used(), 0);
    for budget in [0, required - 1, required] {
        let control = StorageReadControl::with_limit(budget);
        let result = source.image_document_reader(1, &control);
        assert_eq!(result.is_ok(), budget == required);
        drop(result);
        assert_eq!(control.memory().used(), 0);
    }
    for cancelled in 0..3 {
        let original = StorageReadControl::with_limit(1 << 20);
        let adapter = StorageReadControl::with_limit(0);
        let control = StorageReadControl::with_limit(16 << 10);
        let mut imported = builder(&original);
        imported
            .add_image_document(37, &mut fixture(false).as_slice())
            .unwrap();
        let source = crate::ReadOnlySnapshot::new(
            Arc::new(imported.finish().unwrap()) as Arc<dyn InvertedIndex>
        )
        .with_inverted_read_control(&adapter)
        .unwrap();
        let mut reader = source.image_document_reader(37, &control).unwrap().unwrap();
        assert_eq!(reader.read(&mut [0]).unwrap(), 1);
        [&original, &adapter, &control][cancelled]
            .cancellation()
            .cancel();
        assert!(matches!(
            reader.read(&mut [0]),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(control.memory().used(), 0);
        assert!(reader.read(&mut []).is_err());
        drop(reader);
        drop(source);
        assert_eq!(original.memory().used(), 0);
    }
}

#[test]
fn text_image_reader_keeps_captured_occurrences_and_statistics_after_source_loss() {
    let mut source = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    source
        .add_document(1, fields("original β original"))
        .unwrap();
    let expected = encode(&source, 1).unwrap();
    let statistics = source.field_stats("body").unwrap();
    let registry = MemoryCaptureRegistry::new();
    let source = registry
        .cell(Box::new(source) as Box<dyn InvertedIndex>)
        .unwrap();
    let retention = StorageReadControl::with_limit(1 << 20);
    let cut = registry.capture(&retention).unwrap();
    source
        .write()
        .add_document(1, fields("replacement"))
        .unwrap();
    let captured = source.captured(&cut).unwrap();
    drop(source);
    drop(cut);
    drop(registry);
    compare(captured.as_ref().as_ref(), 1, &expected, 3);
    let actual = captured.field_stats("body").unwrap();
    assert_eq!(actual.total_docs, statistics.total_docs);
    assert_eq!(actual.avg_doc_length, statistics.avg_doc_length);
    for term in ["original", "β", "replacement"] {
        assert_eq!(
            actual.doc_freq("body", term),
            statistics.doc_freq("body", term)
        );
    }
    drop(captured);
    assert_eq!(retention.memory().used(), 0);
}
