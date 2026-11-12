//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    image_io::work::Work,
    spill_map::{Builder, Map, Root},
    StorageBackendResult,
};
use std::{
    future::{poll_fn, Future},
    task::{Context, Poll, Waker},
};
use uqa_core::memory::MemoryBudget;

struct SuspendAfter(usize);
impl Work for SuspendAfter {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        poll_fn(|_| {
            if self.0 == 0 {
                Poll::Pending
            } else {
                self.0 -= 1;
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
}

#[test]
fn abandoned_ordered_spill_insertion_cannot_resume_or_publish_partial_state() {
    let mut suspended = 0;
    let mut completed = 0;
    for admitted in 0..16 {
        let memory = MemoryBudget::new(1 << 20);
        let mut builder = Builder::new(&memory, 0);
        builder.insert(1, 7_u64, None).unwrap();
        let outcome = {
            let mut work = SuspendAfter(admitted);
            let future = builder.insert_from(2, 9_u64, None, &mut work);
            let mut future = std::pin::pin!(future);
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
        };
        match outcome {
            Poll::Pending => {
                suspended += 1;
                assert!(builder.insert(3, 11, None).is_err());
                assert!(builder.finish(None).is_err());
            }
            Poll::Ready(result) => {
                completed += 1;
                result.unwrap();
                let map = builder.finish(None).unwrap();
                assert_eq!(map.get(1).unwrap().as_deref(), Some(&7));
                assert_eq!(map.get(2).unwrap().as_deref(), Some(&9));
                drop(map);
            }
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(suspended >= 3 && completed > 0);
}

struct YieldEach;

impl Work for YieldEach {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        let mut pending = true;
        poll_fn(|_| {
            if std::mem::take(&mut pending) {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
}

fn drain(mut future: std::pin::Pin<&mut impl Future<Output = StorageBackendResult<()>>>) -> usize {
    let mut pending = 0;
    loop {
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => {
                result.unwrap();
                return pending;
            }
            Poll::Pending => pending += 1,
        }
        assert!(pending < 100_000);
    }
}

#[test]
fn unordered_spill_and_compaction_keep_private_progress_and_reusable_original_roots() {
    for compact in [false, true] {
        let memory = MemoryBudget::new(4 << 20);
        let mut base = Map::new(&memory, if compact { 0 } else { 1 << 20 });
        for key in 0..48 {
            base.insert(key, key as u64, None).unwrap();
        }
        if compact {
            let Root::Disk(disk) = &mut base.root else {
                panic!("spilled fixture")
            };
            let mut revision = 0;
            while disk.file_bytes() <= 1 << 20 {
                disk.insert(13, &revision, &memory).unwrap();
                revision += 1;
            }
            assert!(revision > 1000);
        } else {
            // Force the next native insertion through resident conversion without replacing its values.
            base.resident_bytes = 0;
            let Root::Memory(resident) = &mut base.root else {
                panic!("resident fixture")
            };
            *resident = resident.clone().into_mutation_budget(&memory.child(0));
        }
        let expected = super::entries(&base);
        let polls = {
            let mut candidate = base.clone();
            let mut work = YieldEach;
            let future = candidate.insert_from(100, 901, None, &mut work);
            let polls = drain(std::pin::pin!(future));
            assert_eq!(*candidate.get(100).unwrap().unwrap(), 901);
            assert_eq!(super::entries(&base), expected);
            polls
        };
        assert!(
            polls > 48,
            "native conversion must retain per-record progress"
        );
        for admitted in [0, 1, 2, 8, polls / 2, polls - 1, polls, polls + 1] {
            let mut candidate = base.clone();
            let outcome = {
                let mut work = SuspendAfter(admitted);
                let future = candidate.insert_from(100, 901, None, &mut work);
                let mut future = std::pin::pin!(future);
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
            };
            if outcome.is_pending() {
                assert_eq!(super::entries(&candidate), expected);
                candidate.insert(100, 902, None).unwrap();
                assert_eq!(*candidate.get(100).unwrap().unwrap(), 902);
            } else {
                let Poll::Ready(result) = outcome else {
                    unreachable!()
                };
                result.unwrap();
                assert_eq!(*candidate.get(100).unwrap().unwrap(), 901);
            }
            assert_eq!(super::entries(&base), expected);
            drop(candidate);
        }
        drop(base);
        assert_eq!(memory.used(), 0);
    }
}
