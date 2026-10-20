//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mutation::rules::RuleIndexBuffer;

#[test]
fn rule_ordinal_lookup_bounds_memory_and_page_io_for_repeated_projection() {
    const COUNT: usize = 131_073;
    let allocated = allocation_counter::measure(|| {
        let mut buffer = RuleIndexBuffer::new(16 * 1024, CancellationToken::new()).unwrap();
        for index in 0..COUNT {
            buffer.push(index * 2 + 1).unwrap();
        }
        let indices = buffer.finish().unwrap();
        let mut lookup = indices.lookup().unwrap();
        drop(indices);
        // Repeated OLD/NEW projection on each row, followed by a separate rule's reverse pass.
        for index in 0..COUNT {
            assert_eq!(lookup.get(index).unwrap(), index * 2 + 1);
            assert_eq!(lookup.get(index).unwrap(), index * 2 + 1);
        }
        for index in (0..COUNT).rev() {
            assert_eq!(lookup.get(index).unwrap(), index * 2 + 1);
        }
        let LookupStorage::Pages(pages) = &lookup.storage else {
            panic!("fragmented ordinals must have positional backing");
        };
        let count = COUNT.div_ceil(PAGE_WORDS);
        assert_eq!(pages.page_writes, count);
        assert_eq!(pages.page_reads, count * 2 - 1);
        assert!(lookup.get(COUNT).is_err());
    });
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
}

#[test]
fn rule_ordinal_lookup_keeps_contiguous_ranges_implicit() {
    for count in [0, 1, 131_072] {
        let mut buffer = RuleIndexBuffer::new(1, CancellationToken::new()).unwrap();
        for index in 17..17 + count {
            buffer.push(index).unwrap();
        }
        let mut lookup = buffer.finish().unwrap().lookup().unwrap();
        assert!(matches!(lookup.storage, LookupStorage::Range(_)));
        for index in (0..count).rev() {
            assert_eq!(lookup.get(index).unwrap(), index + 17);
        }
        assert!(lookup.get(count).is_err());
    }
}

#[test]
fn rule_ordinal_lookup_preserves_duplicates_full_width_and_nonmonotonic_reads() {
    let expected = [7, 8, 9, 2, 2, 3, usize::MAX - 2, usize::MAX - 1];
    let mut buffer = RuleIndexBuffer::new(1, CancellationToken::new()).unwrap();
    for index in expected {
        buffer.push(index).unwrap();
    }
    let mut lookup = buffer.finish().unwrap().lookup().unwrap();
    for index in [7, 0, 3, 4, 1, 6, 5, 2, 7, 0] {
        assert_eq!(lookup.get(index).unwrap(), expected[index]);
    }
}

#[test]
fn rule_ordinal_lookup_propagates_cancellation_counts_and_truncated_pages() {
    let token = CancellationToken::new();
    let indices = RuleIndices::all(3, token.clone());
    let mut lookup = indices.lookup().unwrap();
    token.cancel();
    assert_eq!(lookup.get(0).unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(indices.lookup().err().unwrap().sqlstate(), Some("57014"));
    let mut malformed = RuleIndices::all(3, CancellationToken::new());
    malformed.count = 4;
    assert!(malformed.lookup().is_err());
    let mut buffer = RuleIndexBuffer::new(1, CancellationToken::new()).unwrap();
    for index in 0..=PAGE_WORDS {
        buffer.push(index * 2).unwrap();
    }
    let indices = buffer.finish().unwrap();
    let mut lookup = indices.lookup().unwrap();
    assert_eq!(lookup.get(0).unwrap(), 0);
    let LookupStorage::Pages(pages) = &mut lookup.storage else {
        panic!("fragmented ordinals must have positional backing");
    };
    pages.file.set_len(PAGE_BYTES as u64 + 1).unwrap();
    assert!(lookup.get(PAGE_WORDS).is_err());
    let LookupStorage::Pages(pages) = &lookup.storage else {
        unreachable!();
    };
    assert!(pages.page_start.is_none());
    assert_eq!(lookup.get(0).unwrap(), 0);
}
