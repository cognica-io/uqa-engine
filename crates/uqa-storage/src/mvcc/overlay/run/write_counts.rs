//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic counts of value bytes written into private spill runs.

use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::mvcc) struct Counts {
    pub(in crate::mvcc) bytes: u64,
    pub(in crate::mvcc) copied: u64,
}

thread_local! {
    static WRITES: Cell<Counts> = const { Cell::new(Counts { bytes: 0, copied: 0 }) };
}

pub(super) fn value(bytes: u64, copied: bool) {
    let previous = WRITES.get();
    WRITES.set(Counts {
        bytes: previous.bytes + bytes,
        copied: previous.copied + if copied { bytes } else { 0 },
    });
}

pub(in crate::mvcc) fn take() -> Counts {
    WRITES.replace(Counts::default())
}
