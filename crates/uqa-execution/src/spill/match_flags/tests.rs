//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn growing_flags_preserve_dirty_pages_and_leave_new_ordinals_unmarked() {
    let count = (PAGE_BYTES * 2 + 7) as u64;
    let mut flags = MatchFlags::new(0, "join").unwrap();
    for index in 0..count {
        flags.push_unmarked().unwrap();
        assert!(!flags.is_marked(index).unwrap(), "new ordinal {index}");
        if index % 3 == 0 {
            flags.mark(index).unwrap();
        }
        assert!(flags.is_marked(index + 1).is_err());
    }
    for _ in 0..2 {
        for index in 0..count {
            assert_eq!(flags.is_marked(index).unwrap(), index % 3 == 0);
        }
    }
    assert!(flags.page_writes <= 3, "{}", flags.page_writes);
    assert!(flags.page_reads <= 9, "{}", flags.page_reads);
}

#[test]
fn match_flags_cache_bounds_page_io_and_preserves_union() {
    let count = (PAGE_BYTES * 2 + 7) as u64;
    let mut flags = MatchFlags::new(count, "join").unwrap();
    for stride in [3, 7] {
        for index in (0..count).step_by(stride) {
            flags.mark(index).unwrap();
        }
    }
    for _ in 0..2 {
        for index in 0..count {
            assert_eq!(
                flags.is_marked(index).unwrap(),
                index % 3 == 0 || index % 7 == 0,
                "{index}"
            );
        }
    }
    // Two marking passes write each of three pages once; repeated probes only read.
    assert!(flags.page_writes <= 6, "{}", flags.page_writes);
    assert!(flags.page_reads <= 12, "{}", flags.page_reads);
}

#[test]
fn match_flags_preserve_random_probes_duplicates_and_bounds() {
    let mut flags = MatchFlags::new((PAGE_BYTES * 3 + 1) as u64, "join").unwrap();
    let selected = [
        (PAGE_BYTES * 3) as u64,
        1,
        (PAGE_BYTES + 3) as u64,
        1,
        (PAGE_BYTES * 3) as u64,
    ];
    for index in selected {
        flags.mark(index).unwrap();
        flags.mark(index).unwrap();
    }
    for index in [
        0,
        1,
        PAGE_BYTES as u64,
        (PAGE_BYTES + 3) as u64,
        (PAGE_BYTES * 2) as u64,
        (PAGE_BYTES * 3) as u64,
    ] {
        assert_eq!(flags.is_marked(index).unwrap(), selected.contains(&index));
    }
    assert!(flags.mark(flags.rows).is_err());
    assert!(flags.is_marked(u64::MAX).is_err());
    let mut empty = MatchFlags::new(0, "join").unwrap();
    assert!(empty.is_marked(0).is_err());
    assert_eq!(empty.page_reads, 0);
    assert_eq!(empty.page_writes, 0);
}

#[test]
fn match_flags_do_not_turn_backing_read_errors_into_missing_matches() {
    let mut flags = MatchFlags::new((PAGE_BYTES * 2) as u64, "join").unwrap();
    flags.mark(3).unwrap();
    assert!(!flags.is_marked(PAGE_BYTES as u64).unwrap());
    // Corrupt only this test's owned encrypted temporary file after its first page is flushed.
    std::fs::OpenOptions::new()
        .write(true)
        .open(flags.file.path())
        .unwrap()
        .set_len(0)
        .unwrap();
    assert!(flags.is_marked(3).is_err());
}
