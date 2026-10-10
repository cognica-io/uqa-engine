//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A synthetic provider rejects nested read admission and counts validation windows.

use super::*;
use crate::read_control::StorageReadControl;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(super) struct WindowSnapshot {
    inner: MemoryRecordSnapshot,
    active: AtomicBool,
    windows: AtomicUsize,
    points: AtomicUsize,
    requests: AtomicUsize,
    cancel_after: Option<usize>,
}

impl WindowSnapshot {
    pub(super) fn new(inner: MemoryRecordSnapshot) -> Self {
        Self {
            inner,
            active: AtomicBool::new(false),
            windows: AtomicUsize::new(0),
            points: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
            cancel_after: None,
        }
    }

    pub(super) fn cancelling_after(mut self, count: usize) -> Self {
        self.cancel_after = Some(count);
        self
    }

    pub(super) fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    pub(super) fn assert_reads(&self, windows: usize, points: usize) {
        assert_eq!(self.windows.load(Ordering::Relaxed), windows);
        assert_eq!(self.points.load(Ordering::Relaxed), points);
        assert!(!self.active.load(Ordering::Relaxed));
    }
}

impl CommittedRecordSnapshot for WindowSnapshot {
    fn sequence(&self) -> CommitSequence {
        self.inner.sequence()
    }

    fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
        assert!(!self.active.load(Ordering::Relaxed));
        control.check()?;
        Ok(self.inner.get(key))
    }

    fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<RecordPage> {
        assert!(!self.active.load(Ordering::Relaxed));
        self.inner.scan(prefix, after, limit, control)
    }

    fn metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordMetadata>> {
        assert!(!self.active.load(Ordering::Relaxed));
        self.points.fetch_add(1, Ordering::Relaxed);
        self.inner.metadata(key, control)
    }

    fn visit_metadata(
        &self,
        requests: &mut dyn RecordMetadataRequests,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        if !requests.advance()? {
            return Ok(());
        }
        assert!(!self.active.swap(true, Ordering::Relaxed));
        self.windows.fetch_add(1, Ordering::Relaxed);
        let result = (|| loop {
            control.check()?;
            let count = self.requests.fetch_add(1, Ordering::Relaxed) + 1;
            requests.accept(self.inner.metadata(requests.key(), control)?)?;
            if self.cancel_after == Some(count) {
                control.cancellation().cancel();
            }
            if !requests.advance()? {
                return Ok(());
            }
        })();
        self.active.store(false, Ordering::Relaxed);
        result
    }
}
