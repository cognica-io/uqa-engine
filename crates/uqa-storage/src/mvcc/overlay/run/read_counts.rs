//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Thread-local counters for deterministic spill-read complexity assertions.

use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::mvcc) struct Counts {
    pub(in crate::mvcc) entries: usize,
    pub(in crate::mvcc) values: usize,
}

thread_local! {
    static READS: Cell<Counts> = const { Cell::new(Counts { entries: 0, values: 0 }) };
}

pub(super) fn entry() {
    let counts = READS.get();
    READS.set(Counts {
        entries: counts.entries + 1,
        ..counts
    });
}

pub(super) fn value() {
    let counts = READS.get();
    READS.set(Counts {
        values: counts.values + 1,
        ..counts
    });
}

pub(in crate::mvcc) fn take() -> Counts {
    READS.replace(Counts::default())
}
