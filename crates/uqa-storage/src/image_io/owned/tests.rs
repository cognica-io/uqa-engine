//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::memory::Budgeted;

#[derive(Clone, Copy)]
enum Failure {
    None,
    Open,
    OpenPanic,
    Prepare,
    PreparePanic,
    Read,
    ReadPanic,
}

#[derive(Default)]
struct Observed {
    opened: AtomicUsize,
    dropped: AtomicUsize,
    clock: AtomicUsize,
    reader_drop: AtomicUsize,
    owner_drop: AtomicUsize,
}

struct Owner {
    bytes: Budgeted<Vec<u8>>,
    original: StorageReadControl,
    observed: Arc<Observed>,
    failure: Failure,
}

impl Owner {
    fn new(
        length: usize,
        failure: Failure,
        original: &StorageReadControl,
        observed: &Arc<Observed>,
    ) -> Self {
        let memory = original.memory().reserve(length).unwrap();
        Self {
            bytes: Budgeted::new(vec![39; length], memory),
            original: original.clone(),
            observed: observed.clone(),
            failure,
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.observed.dropped.fetch_add(1, Ordering::SeqCst);
        self.observed.owner_drop.store(
            self.observed.clock.fetch_add(1, Ordering::SeqCst) + 1,
            Ordering::SeqCst,
        );
    }
}

struct Borrowed<'a> {
    owner: &'a Owner,
    position: usize,
    remaining: usize,
}

impl Drop for Borrowed<'_> {
    fn drop(&mut self) {
        self.owner.observed.reader_drop.store(
            self.owner.observed.clock.fetch_add(1, Ordering::SeqCst) + 1,
            Ordering::SeqCst,
        );
    }
}

impl ImageOwner for Owner {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.observed.opened.fetch_add(1, Ordering::SeqCst);
        match self.failure {
            Failure::Open => return Err(StorageBackendError::Other("open image".into())),
            Failure::OpenPanic => panic!("open image unwind"),
            _ => {}
        }
        ImageReader::new(
            Borrowed {
                owner: self,
                position: 0,
                remaining: 3,
            },
            control,
        )?
        .with_source_control(Some(&self.original), control)
    }
}

impl ImageRead for Borrowed<'_> {
    fn prepare(&mut self, maximum: usize) -> StorageBackendResult<ImagePreparation> {
        match self.owner.failure {
            Failure::Prepare => return Err(StorageBackendError::Other("prepare image".into())),
            Failure::PreparePanic => panic!("prepare image unwind"),
            _ => {}
        }
        let positions = maximum.min(self.remaining);
        self.remaining -= positions;
        Ok(ImagePreparation {
            positions,
            ready: self.remaining == 0,
        })
    }
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        match self.owner.failure {
            Failure::Read => return Err(StorageBackendError::Other("read image".into())),
            Failure::ReadPanic => panic!("read image unwind"),
            _ => {}
        }
        let count = output.len().min(self.owner.bytes.len() - self.position);
        output[..count].copy_from_slice(&self.owner.bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[test]
fn owned_image_readers_move_live_borrows_and_release_their_source_at_eof() {
    let original = StorageReadControl::with_limit(2 << 20);
    let invoking = StorageReadControl::with_limit(64 << 10);
    let observed = Arc::new(Observed::default());
    let owner = Owner::new((1 << 20) + 7, Failure::None, &original, &observed);
    let mut reader = ImageReader::from_owner(owner, &invoking).unwrap();
    assert_eq!(observed.opened.load(Ordering::SeqCst), 0);
    assert!(!reader.prepare(1).unwrap().ready);
    assert_eq!(observed.opened.load(Ordering::SeqCst), 1);
    let mut reader = std::thread::spawn(move || {
        let mut output = vec![0; 32768];
        let measured = allocation_counter::measure(|| {
            assert!(!reader.prepare(1).unwrap().ready);
            assert!(reader.prepare(1).unwrap().ready);
            let mut total = 0;
            loop {
                let count = reader.read(&mut output).unwrap();
                assert!(count <= WINDOW);
                if count == 0 {
                    break;
                }
                assert!(output[..count].iter().all(|byte| *byte == 39));
                total += count;
            }
            assert_eq!(total, (1 << 20) + 7);
        });
        assert_eq!(measured.bytes_total, 0, "{measured:?}");
        reader
    })
    .join()
    .unwrap();
    assert_eq!(original.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
    assert_eq!(observed.dropped.load(Ordering::SeqCst), 1);
    assert!(
        observed.reader_drop.load(Ordering::SeqCst) < observed.owner_drop.load(Ordering::SeqCst)
    );
    invoking.cancellation().cancel();
    original.cancellation().cancel();
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
    assert!(reader.prepare(1).unwrap().ready);
}

#[test]
fn owned_image_readers_retire_open_prepare_read_failures_and_unwinds() {
    for failure in [
        Failure::Open,
        Failure::OpenPanic,
        Failure::Prepare,
        Failure::PreparePanic,
        Failure::Read,
        Failure::ReadPanic,
    ] {
        let original = StorageReadControl::with_limit(4096);
        let invoking = StorageReadControl::with_limit(64 << 10);
        let observed = Arc::new(Observed::default());
        let mut reader =
            ImageReader::from_owner(Owner::new(4096, failure, &original, &observed), &invoking)
                .unwrap();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.read(&mut [0; 1])));
        if matches!(
            failure,
            Failure::OpenPanic | Failure::PreparePanic | Failure::ReadPanic
        ) {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert_eq!(observed.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(original.memory().used(), 0);
        assert_eq!(invoking.memory().used(), 0);
        assert!(reader.prepare(1).is_err());
        assert!(reader.read(&mut [0; 1]).is_err());
    }
}

#[test]
fn owned_image_readers_preserve_original_cancellation_and_unread_drop() {
    for stage in 0..4 {
        let original = StorageReadControl::with_limit(4096);
        let invoking = StorageReadControl::with_limit(64 << 10);
        let observed = Arc::new(Observed::default());
        let mut reader = ImageReader::from_owner(
            Owner::new(4096, Failure::None, &original, &observed),
            &invoking,
        )
        .unwrap();
        if stage == 0 {
            drop(reader);
        } else {
            if stage > 1 {
                assert!(!reader.prepare(1).unwrap().ready);
            }
            original.cancellation().cancel();
            let error = if stage == 3 {
                reader.read(&mut []).unwrap_err()
            } else {
                reader.prepare(1).unwrap_err()
            };
            assert!(matches!(error, StorageBackendError::Cancelled(_)));
            assert!(reader.read(&mut [0; 1]).is_err());
        }
        assert_eq!(observed.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(original.memory().used(), 0);
        assert_eq!(invoking.memory().used(), 0);
        if stage == 0 {
            assert_eq!(observed.opened.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn owned_image_reader_admission_covers_every_allocation_before_reading() {
    let original = StorageReadControl::with_limit(4096);
    let observed = Arc::new(Observed::default());
    let probe = StorageReadControl::with_limit(64 << 10);
    let mut reader = ImageReader::from_owner(
        Owner::new(4096, Failure::None, &original, &observed),
        &probe,
    )
    .unwrap();
    let construction = probe.memory().used();
    reader.prepare(1).unwrap();
    let complete = probe.memory().used();
    drop(reader);
    for limit in [
        0,
        size_of::<Mailbox>() - 1,
        construction - 1,
        construction,
        complete - 1,
        complete,
    ] {
        let invoking = StorageReadControl::with_limit(limit);
        let observed = Arc::new(Observed::default());
        let result = ImageReader::from_owner(
            Owner::new(4096, Failure::None, &original, &observed),
            &invoking,
        );
        match result {
            Err(error) => {
                assert!(matches!(error, StorageBackendError::Memory(_)));
                assert!(limit < construction);
            }
            Ok(mut reader) => {
                let result = reader.read(&mut [0; 1]);
                if limit == complete {
                    assert_eq!(result.unwrap(), 1);
                } else {
                    assert!(matches!(result, Err(StorageBackendError::Memory(_))));
                }
            }
        }
        assert_eq!(observed.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(original.memory().used(), 0);
        assert_eq!(invoking.memory().used(), 0);
        assert!(invoking.memory().peak() <= limit);
    }
}
