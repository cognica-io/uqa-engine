//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::PathIndexImageSnapshot;
use uqa_storage::{backup::*, mvcc::DatabaseId};

struct PathUnit {
    cut: BackupCut,
    reader: ImageReader<'static>,
    selected: bool,
    ended: bool,
    statistics: BackupSourceStatistics,
}

impl BackupImageSource for PathUnit {
    fn cut(&self) -> BackupCut {
        self.cut
    }
    fn statistics(&self) -> BackupSourceStatistics {
        self.statistics
    }
    fn advance(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        if self.selected {
            assert!(self.ended);
            return Ok(BackupSourceStep::Complete);
        }
        let progress = self.reader.prepare(maximum)?;
        assert!(progress.ready);
        assert_eq!(progress.positions, 0);
        self.statistics.visited_positions += 1;
        self.selected = true;
        Ok(BackupSourceStep::Unit)
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.selected.then_some(BackupSourceUnit {
            address: UnitAddress {
                namespace: NamespaceId::from_bytes([7; 16]).unwrap(),
                generation: 0,
                family: 1,
                key: b"g::paths",
            },
            kind: UnitKind::Record,
            decoded_bytes: None,
        })
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let count = self.reader.read(output)?;
        self.statistics.copied_payload_bytes += count as u64;
        self.ended = count == 0 && !output.is_empty();
        Ok(count)
    }
}

#[test]
fn path_images_restore_standalone_full_and_incremental_protected_backups() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(512 << 10);
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        Protection::Encrypted {
            domain: ProtectionId::from_bytes([1; 16]).unwrap(),
            key: BackupKey::new([42; 32]),
        },
        BackupLimits::new(4096, 4096).unwrap(),
        &control,
    )
    .unwrap();
    let expected = [path(
        &[&"label".repeat(8192)],
        &(0..2048).map(|id| (id, id + 1)).collect::<Vec<_>>(),
    )];
    let mut source = materialized(&expected);
    let original = StorageReadControl::with_limit(4 << 20);
    let snapshot = source
        .capture_native_memory_with_control(&original)
        .unwrap();
    let full = capture(&repository, &snapshot, &mut source, None, false, &control);
    let unchanged = capture(
        &repository,
        &snapshot,
        &mut source,
        Some(full.backup()),
        true,
        &control,
    );
    drop(snapshot);
    assert_eq!(original.memory().used(), 0);
    assert!(source.lookup(&expected[0].sequence).unwrap().is_none());
    let changed_paths = [path(&["replacement"], &[(u64::MAX, 0)])];
    source = materialized(&changed_paths);
    let snapshot = source
        .capture_native_memory_with_control(&original)
        .unwrap();
    let changed = capture(
        &repository,
        &snapshot,
        &mut source,
        Some(unchanged.backup()),
        false,
        &control,
    );
    drop((source, snapshot));
    assert_eq!(original.memory().used(), 0);
    repository.remove_backup(full.backup(), &control).unwrap();
    restore(&repository, unchanged.backup(), &expected, &control);
    repository
        .remove_backup(unchanged.backup(), &control)
        .unwrap();
    restore(&repository, changed.backup(), &changed_paths, &control);
    assert_eq!(control.memory().used(), 0);
}

fn capture(
    repository: &LocalRepository,
    snapshot: &PathIndexImageSnapshot,
    source: &mut PathIndex,
    parent: Option<BackupId>,
    reused: bool,
    control: &StorageReadControl,
) -> BackupRoot {
    let image = PathUnit {
        cut: BackupCut {
            source: DatabaseId::from_bytes([2; 16]),
            snapshot: SnapshotId::new().unwrap(),
            format: ImageFormat::new(ImageProvider::Memory, 1).unwrap(),
            sequence: None,
        },
        reader: snapshot.clone().into_image_reader(control).unwrap(),
        selected: false,
        ended: false,
        statistics: BackupSourceStatistics::default(),
    };
    let mut job = repository
        .begin_image_backup(image, parent, 4 << 20, 4 << 20, control)
        .unwrap();
    let mut replaced = false;
    let mut calls = 0;
    loop {
        calls += 1;
        assert!(calls < 100_000);
        let before = job.progress();
        let step = job.advance(1, 1021).unwrap();
        let after = job.progress();
        assert!(after.source.visited_positions - before.source.visited_positions <= 1);
        assert!(after.source.copied_payload_bytes - before.source.copied_payload_bytes <= 1021);
        assert!(after.image.hashed_payload_bytes - before.image.hashed_payload_bytes <= 1021);
        if !replaced && after.source.copied_payload_bytes > 0 {
            *source = PathIndex::default();
            replaced = true;
        }
        if step == BackupCaptureStep::Prepared {
            break;
        }
        assert_eq!(step, BackupCaptureStep::Pending);
    }
    assert!(replaced);
    let report = job.report().unwrap();
    assert_eq!(report.source.visited_positions, 1);
    assert_eq!(
        report.source.copied_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(
        report.image.hashed_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(report.image.reused_units, u64::from(reused));
    let root = job.publish().unwrap();
    drop(job);
    assert_eq!(control.memory().used(), 0);
    root
}

fn restore(
    repository: &LocalRepository,
    backup: BackupId,
    expected: &[CapturedPath],
    control: &StorageReadControl,
) {
    let lease = repository.open_backup(backup, control).unwrap();
    let mut image = lease.read_image(4 << 20, control).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 1021];
    loop {
        match image.advance(1021).unwrap() {
            BackupReadStep::Pending => {}
            BackupReadStep::Unit => loop {
                let count = image.read_current(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
            },
            BackupReadStep::Complete => break,
        }
    }
    assert!(image.verified().is_some());
    assert_eq!(bytes, frame(expected));
    let destination = StorageReadControl::with_limit(4 << 20);
    let restored = PathIndex::read_image(&mut bytes.as_slice(), &destination).unwrap();
    assert_paths(&restored, expected);
    drop(restored);
    assert_eq!(destination.memory().used(), 0);
}
