//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const COUNT: usize = 131_072;
const MAX_LIVE_BYTES: u64 = 512 * 1024;

fn verify(indices: &RuleIndices, stride: usize) {
    assert_eq!(indices.len(), COUNT);
    for _ in 0..2 {
        let mut count = 0;
        for (position, index) in indices.reader().unwrap().enumerate() {
            assert_eq!(index.unwrap(), position * stride);
            count += 1;
        }
        assert_eq!(count, COUNT);
    }
}

#[test]
fn all_rule_indices_bound_metadata_and_preserve_repeatable_order() {
    let allocated = allocation_counter::measure(|| {
        let indices = RuleIndices::all(COUNT, CancellationToken::new());
        verify(&indices, 1);
    });
    assert!(allocated.bytes_max < MAX_LIVE_BYTES, "{allocated:?}");
}

#[test]
fn sparse_rule_indices_bound_metadata_and_preserve_repeatable_order() {
    let allocated = allocation_counter::measure(|| {
        let mut buffer = RuleIndexBuffer::new(16 * 1024, CancellationToken::new()).unwrap();
        for index in 0..COUNT {
            buffer.push(index * 2).unwrap();
        }
        verify(&buffer.finish().unwrap(), 2);
    });
    assert!(allocated.bytes_max < MAX_LIVE_BYTES, "{allocated:?}");
}

#[test]
fn rule_indices_preserve_duplicates_reverse_order_and_full_width_ordinals() {
    let expected = [7, 8, 9, 2, 2, 3, usize::MAX - 2, usize::MAX - 1];
    let mut buffer = RuleIndexBuffer::new(1, CancellationToken::new()).unwrap();
    for index in expected {
        buffer.push(index).unwrap();
    }
    let indices = buffer.finish().unwrap();
    assert!(matches!(&indices.storage, IndexStorage::Spill(rows) if rows.has_spilled()));
    for _ in 0..2 {
        assert_eq!(
            indices
                .reader()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            expected
        );
    }
}

#[test]
fn empty_and_contiguous_rule_indices_do_not_create_spill_records() {
    for count in [0, 1, COUNT] {
        let mut buffer = RuleIndexBuffer::new(1, CancellationToken::new()).unwrap();
        for index in 23..23 + count {
            buffer.push(index).unwrap();
        }
        let indices = buffer.finish().unwrap();
        assert_eq!(indices.len(), count);
        assert!(matches!(indices.storage, IndexStorage::Range(_)));
        assert!(indices
            .reader()
            .unwrap()
            .map(Result::unwrap)
            .eq(23..23 + count));
    }
}

#[test]
fn rule_indices_propagate_cancellation_and_stop_failed_readers() {
    let token = CancellationToken::new();
    let indices = RuleIndices::all(3, token.clone());
    let mut reader = indices.reader().unwrap();
    assert_eq!(reader.next().unwrap().unwrap(), 0);
    let mut buffer = RuleIndexBuffer::new(1, token.clone()).unwrap();
    token.cancel();
    assert_eq!(
        reader.next().unwrap().unwrap_err().sqlstate(),
        Some("57014")
    );
    assert!(reader.next().is_none());
    assert_eq!(indices.reader().err().unwrap().sqlstate(), Some("57014"));
    assert_eq!(buffer.push(0).unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(buffer.finish().err().unwrap().sqlstate(), Some("57014"));
}

#[test]
fn rule_indices_reject_malformed_ranges_counts_and_unaddressable_ordinals() {
    use uqa_core::Value;

    for values in [
        Vec::new(),
        vec![Value::Null, Value::Null],
        vec![Value::Bytes(vec![0]), encode_prepared_doc_id(1)],
        vec![encode_prepared_doc_id(3), encode_prepared_doc_id(3)],
        vec![encode_prepared_doc_id(4), encode_prepared_doc_id(3)],
    ] {
        assert!(decode_range(PhysicalRow::from_values(values)).is_err());
    }
    for declared in [0, 2] {
        let mut indices = RuleIndices::all(1, CancellationToken::new());
        indices.count = declared;
        let mut rows = indices.reader().unwrap();
        if declared == 2 {
            assert_eq!(rows.next().unwrap().unwrap(), 0);
        }
        assert!(rows.next().unwrap().is_err());
        assert!(rows.next().is_none());
    }
    let mut buffer = RuleIndexBuffer::new(1, CancellationToken::new()).unwrap();
    assert!(buffer.push(usize::MAX).is_err());
    buffer.count = usize::MAX;
    assert!(buffer.push(0).is_err());
    let mut buffer = RuleIndexBuffer::new(1, CancellationToken::new()).unwrap();
    buffer.push(0).unwrap();
    buffer.push(2).unwrap();
    let mut indices = buffer.finish().unwrap();
    indices.count = 1;
    let mut rows = indices.reader().unwrap();
    assert_eq!(rows.next().unwrap().unwrap(), 0);
    assert!(rows.next().unwrap().is_err());
    assert!(rows.next().is_none());
}
