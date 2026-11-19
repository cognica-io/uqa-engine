//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::work::Work;
use std::{
    future::{poll_fn, Future},
    task::{Context, Poll, Waker},
};

struct YieldEach;

fn read_limits() -> DiskANNReadLimits {
    DiskANNReadLimits {
        resident_bytes: 1 << 20,
        max_record_bytes: 1 << 20,
        ..limits(0)
    }
}
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

fn drain<T>(future: impl Future<Output = StorageBackendResult<T>>) -> (T, usize) {
    let mut future = std::pin::pin!(future);
    let mut pending = 0;
    loop {
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => return (result.unwrap(), pending),
            Poll::Pending => pending += 1,
        }
        assert!(pending < 100_000);
    }
}

#[test]
fn memory_reader_preparation_resumes_large_records_and_retires_partial_allocations() {
    let fixture = fixture(4096, 4, 2);
    let original = StorageReadControl::with_limit(8 << 20);
    let source = Arc::new(fixture.memory(original.memory(), &original).unwrap());
    let original_bytes = original.memory().used();
    let polls = {
        let control = StorageReadControl::with_limit(8 << 20);
        let mut work = YieldEach;
        let (reader, polls) = drain(DiskANNReader::open_from(
            RecordInput::Memory(source.clone()),
            4096,
            parameters(4096),
            read_limits(),
            &control,
            &mut work,
        ));
        assert_eq!(reader.manifest(), &fixture.manifest);
        for id in 0..4 {
            let node = reader.read_node(id, &control).unwrap();
            assert_eq!(node.doc_id(), id + 10);
            assert_eq!(node.vector()[0], if id % 2 == 0 { 1.0 } else { -1.0 });
            assert!(node.vector()[1..].iter().all(|value| *value == 0.0));
        }
        drop(reader);
        assert_eq!(control.memory().used(), 0);
        polls
    };
    assert!(
        polls > 32,
        "large-record copying and validation must yield internally"
    );
    for admitted in 0..polls {
        let control = StorageReadControl::with_limit(8 << 20);
        let outcome = {
            let mut work = SuspendAfter(admitted);
            let future = DiskANNReader::open_from(
                RecordInput::Memory(source.clone()),
                4096,
                parameters(4096),
                read_limits(),
                &control,
                &mut work,
            );
            let mut future = std::pin::pin!(future);
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
        };
        assert!(
            outcome.is_pending(),
            "suspend at preparation checkpoint {admitted}"
        );
        assert_eq!(control.memory().used(), 0, "checkpoint {admitted}");
        assert_eq!(original.memory().used(), original_bytes);
    }
    for failure in ["cancel", "memory"] {
        let control = StorageReadControl::with_limit(8 << 20);
        {
            let mut work = YieldEach;
            let future = DiskANNReader::open_from(
                RecordInput::Memory(source.clone()),
                4096,
                parameters(4096),
                read_limits(),
                &control,
                &mut work,
            );
            let mut future = std::pin::pin!(future);
            assert!(future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending());
            if failure == "cancel" {
                control.cancellation().cancel();
            }
            let held = (failure == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            loop {
                if let Poll::Ready(result) = future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                {
                    match (failure, result) {
                        ("cancel", Err(StorageBackendError::Cancelled(_)))
                        | ("memory", Err(StorageBackendError::Memory(_))) => (),
                        _ => panic!("preparation did not propagate {failure}"),
                    }
                    break;
                }
            }
            drop(held);
        }
        assert_eq!(control.memory().used(), 0);
    }
    drop(source);
    assert_eq!(original.memory().used(), 0);
}

#[test]
fn abandoned_native_sealing_rejects_reentry_and_releases_partial_node_and_codebook_state() {
    let fixture = fixture(4096, 4, 2);
    let codebook = &fixture.records[0].1;
    assert_eq!(fixture.records[0].0, DiskANNRecordKey::Codebook);
    assert!(codebook.len() > PAGE_BYTES * 4);
    let last_fragment = u64::from(fixture.manifest.layout().node_address(0).unwrap().fragments - 1);
    assert!(last_fragment > 1);
    for graph in [false, true] {
        let setup = |control: &StorageReadControl| {
            let mut sealer = DiskANNArtifactSealer::new(fixture.manifest, control).unwrap();
            if graph {
                for id in 0..last_fragment {
                    sealer.graph_page(id, &fixture.graph[id as usize]).unwrap();
                }
            }
            sealer
        };
        let polls = {
            let control = StorageReadControl::with_limit(8 << 20);
            let mut sealer = setup(&control);
            let mut work = YieldEach;
            let ((), polls) = if graph {
                drain(sealer.graph_page_from(
                    last_fragment,
                    &fixture.graph[last_fragment as usize],
                    &mut work,
                ))
            } else {
                drain(sealer.codebook_from(codebook, &mut work))
            };
            drop(sealer);
            assert_eq!(control.memory().used(), 0);
            polls
        };
        assert!(polls > 8);
        for admitted in 0..polls {
            let control = StorageReadControl::with_limit(8 << 20);
            let mut sealer = setup(&control);
            let outcome = {
                let mut work = SuspendAfter(admitted);
                if graph {
                    let future = sealer.graph_page_from(
                        last_fragment,
                        &fixture.graph[last_fragment as usize],
                        &mut work,
                    );
                    let mut future = std::pin::pin!(future);
                    future
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                } else {
                    let future = sealer.codebook_from(codebook, &mut work);
                    let mut future = std::pin::pin!(future);
                    future
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                }
            };
            assert!(outcome.is_pending());
            let error = sealer.codebook(codebook).unwrap_err();
            assert!(error.to_string().contains("previously failed"));
            assert!(sealer.finish().is_err());
            assert_eq!(
                control.memory().used(),
                0,
                "graph={graph}, checkpoint={admitted}"
            );
        }
    }
}
