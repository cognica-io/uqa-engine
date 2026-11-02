//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

struct Image {
    cut: BackupCut,
    current: Option<u64>,
    key: [u8; 8],
    offset: usize,
    units: u64,
    fault: u8,
    control: StorageReadControl,
    dropped: Arc<AtomicBool>,
    statistics: BackupSourceStatistics,
}

impl Image {
    fn new(cut: BackupCut, units: u64, fault: u8, control: &StorageReadControl) -> Self {
        Self {
            cut,
            current: None,
            key: [0; 8],
            offset: 0,
            units,
            fault,
            control: control.clone(),
            dropped: Arc::new(AtomicBool::new(false)),
            statistics: BackupSourceStatistics::default(),
        }
    }
    fn bytes(&self) -> usize {
        match self.current.unwrap() {
            0 => 0,
            1 => (2 << 20) + 7,
            _ => 32,
        }
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl BackupImageSource for Image {
    fn cut(&self) -> BackupCut {
        self.cut
    }
    fn statistics(&self) -> BackupSourceStatistics {
        self.statistics
    }
    fn advance(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        assert!(maximum > 0 && maximum <= 128);
        self.control.cancellation().check()?;
        self.statistics.visited_positions += 1;
        if self.statistics.visited_positions % 2 == 1 {
            return Ok(BackupSourceStep::Pending);
        }
        if self.fault == 4 {
            return Err(BackupError::Invalid("injected source failure"));
        }
        let index = self.current.map_or(0, |index| index + 1);
        if index == self.units {
            return Ok(BackupSourceStep::Complete);
        }
        self.current = Some(index);
        self.offset = 0;
        self.key = if self.fault == 3 { 0_u64 } else { index }.to_be_bytes();
        Ok(BackupSourceStep::Unit)
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.current?;
        let mut length = self.bytes() as u64;
        if self.current == Some(1) {
            match self.fault {
                1 => length += 1,
                2 => length -= 1,
                _ => (),
            }
        }
        Some(BackupSourceUnit {
            address: UnitAddress {
                namespace: NamespaceId::from_bytes([7; 16]).unwrap(),
                generation: 0,
                family: 1,
                key: &self.key,
            },
            kind: UnitKind::Record,
            decoded_bytes: length,
        })
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        assert!(
            output.len() <= 4096,
            "capture reads follow the configured frame window"
        );
        let copied = output.len().min(self.bytes() - self.offset);
        output[..copied].fill(self.current.unwrap() as u8);
        self.offset += copied;
        self.statistics.copied_payload_bytes += copied as u64;
        if self.fault == 5 && self.offset > 4096 {
            self.control.cancellation().cancel();
        }
        Ok(copied)
    }
}

#[test]
fn backup_capture_packs_streamed_units_under_fixed_memory_and_verifies_after_source_drop() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(128 * 1024);
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let source = Image::new(cut(), 514, 0, &control);
    let dropped = source.dropped.clone();
    let mut operation = repository
        .begin_backup(source.cut(), None, 16 << 20, &control)
        .unwrap();
    let report = operation.prepare_image(source, 2 << 20, &control).unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(report.inventory.units, 514);
    assert_eq!(report.inventory.decoded_bytes, (2 << 20) + 7 + 512 * 32);
    assert_eq!(
        report.source.copied_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(report.source.visited_positions, 1030);
    let root = operation.publish(&control).unwrap();
    drop(operation);
    assert!(
        std::fs::read_dir(directory.path().join("backup/objects"))
            .unwrap()
            .count()
            <= 20,
        "small records share immutable packs"
    );
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let mut reader = lease.read_image(2 << 20, &control).unwrap();
    let mut units = 0_u64;
    loop {
        match reader.advance(4096).unwrap() {
            BackupReadStep::Complete => break,
            BackupReadStep::Pending => continue,
            BackupReadStep::Unit => (),
        }
        let entry = reader.current_unit().unwrap();
        assert_eq!(entry.address().key, units.to_be_bytes());
        let mut copied = 0;
        let mut buffer = [0; 1024];
        loop {
            let n = reader.read_current(&mut buffer).unwrap();
            if n == 0 {
                break;
            }
            assert!(buffer[..n].iter().all(|byte| *byte == units as u8));
            copied += n;
        }
        assert_eq!(
            copied,
            match units {
                0 => 0,
                1 => (2 << 20) + 7,
                _ => 32,
            }
        );
        units += 1;
    }
    assert_eq!(units, 514);
    assert_eq!(reader.verified().unwrap().root(), root);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= 128 * 1024);
}

#[test]
fn backup_capture_failures_retire_source_and_cannot_publish_partial_images() {
    for fault in 1..=9 {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = LocalRepository::create(
            directory.path().join("backup"),
            protected(),
            limits(),
            &control,
        )
        .unwrap();
        let source = Image::new(cut(), 3, fault, &control);
        let dropped = source.dropped.clone();
        let cut = if fault == 6 { cut() } else { source.cut() };
        let limit = if fault == 7 { 1024 } else { 16 << 20 };
        let mut operation = repository.begin_backup(cut, None, limit, &control).unwrap();
        if fault == 9 {
            *repository.fault.lock() = Some(Fault::ObjectSync);
        }
        let workspace = if fault == 8 { 1 } else { 2 << 20 };
        let error = operation
            .prepare_image(source, workspace, &control)
            .unwrap_err();
        if fault == 5 {
            assert!(matches!(error, BackupError::Cancelled(_)));
        }
        if fault == 8 {
            assert!(matches!(
                error,
                BackupError::Limit {
                    resource: "capture workspace",
                    ..
                }
            ));
        }
        assert!(dropped.load(Ordering::SeqCst));
        assert!(matches!(
            operation.publish(&control),
            Err(BackupError::Finished)
        ));
        assert_eq!(control.memory().used(), 0);
        assert_eq!(
            std::fs::read_dir(directory.path().join("backup/roots"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(operation.cancel().unwrap(), None);
    }
}

#[test]
fn backup_capture_accepts_an_empty_provider_image() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let source = Image::new(cut(), 0, 0, &control);
    let mut operation = repository
        .begin_backup(source.cut(), None, 1 << 20, &control)
        .unwrap();
    assert_eq!(
        operation
            .prepare_image(source, 1 << 20, &control)
            .unwrap()
            .inventory
            .units,
        0
    );
    let root = operation.publish(&control).unwrap();
    repository
        .open_backup(root.backup(), &control)
        .unwrap()
        .verify(1 << 20, &control)
        .unwrap();
    assert_eq!(control.memory().used(), 0);
}
