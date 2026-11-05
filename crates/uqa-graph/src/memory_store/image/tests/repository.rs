//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::GraphStoreHandle;
use std::sync::Arc;
use uqa_storage::{backup::*, image_io::ImageReader, mvcc::DatabaseId};

struct GraphUnit<'a> {
    cut: BackupCut,
    reader: ImageReader<'a>,
    selected: bool,
    ended: bool,
    statistics: BackupSourceStatistics,
}

impl BackupImageSource for GraphUnit<'_> {
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
        self.statistics.visited_positions += progress.positions as u64;
        if !progress.ready {
            return Ok(BackupSourceStep::Pending);
        }
        self.selected = true;
        Ok(BackupSourceStep::Unit)
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.selected.then_some(BackupSourceUnit {
            address: UnitAddress {
                namespace: NamespaceId::from_bytes([7; 16]).unwrap(),
                generation: 0,
                family: 1,
                key: b"graph",
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
fn graph_images_stream_through_protected_full_and_incremental_repositories() {
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
    let mut store = source();
    let mut vertex = store.get_vertex(1).unwrap().clone();
    vertex
        .properties
        .insert("blob".into(), Value::Bytes(vec![91; 65549]));
    store.insert_raw_vertex(vertex).unwrap();
    let expected = encode(&store);
    let original = StorageReadControl::with_limit(4 << 20);
    let mut source = Arc::new(GraphStoreHandle::Memory(store));
    let snapshot = source.capture_image_with_control(&original).unwrap();
    let mut roots = Vec::new();
    let mut parent = None;
    for _ in 0..2 {
        let root = capture(&repository, &snapshot, &mut source, parent, &control);
        assert_eq!(root.summary().decoded_bytes, expected.len() as u64);
        parent = Some(root.backup());
        roots.push(root);
    }
    drop((source, snapshot));
    assert_eq!(original.memory().used(), 0);
    repository
        .remove_backup(roots[0].backup(), &control)
        .unwrap();
    let lease = repository.open_backup(roots[1].backup(), &control).unwrap();
    let mut image = lease.read_image(4 << 20, &control).unwrap();
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
    assert_eq!(bytes, expected);
    let destination = StorageReadControl::with_limit(4 << 20);
    let restored = decode(&bytes, &destination);
    assert!(restored.get_vertex(99).is_none());
    assert_eq!(
        restored
            .neighbors(1, Some("r"), Direction::Out, "g")
            .unwrap(),
        [2]
    );
    assert!(
        matches!(&restored.get_vertex(1).unwrap().properties["blob"], Value::Bytes(bytes) if bytes.len() == 65549 && bytes.iter().all(|byte| *byte == 91))
    );
    drop(restored);
    assert_eq!(destination.memory().used(), 0);
}

fn capture(
    repository: &LocalRepository,
    snapshot: &crate::GraphImageSnapshot,
    source: &mut Arc<GraphStoreHandle>,
    parent: Option<BackupId>,
    control: &StorageReadControl,
) -> BackupRoot {
    let image = GraphUnit {
        cut: BackupCut {
            source: DatabaseId::from_bytes([2; 16]),
            snapshot: SnapshotId::new().unwrap(),
            format: ImageFormat::new(ImageProvider::Memory, 1).unwrap(),
            sequence: None,
        },
        reader: snapshot.image_reader(control).unwrap(),
        selected: false,
        ended: false,
        statistics: BackupSourceStatistics::default(),
    };
    let mut job = repository
        .begin_image_backup(image, parent, 4 << 20, 4 << 20, control)
        .unwrap();
    let mut changed = false;
    let mut calls = 0;
    loop {
        calls += 1;
        assert!(calls < 100_000, "finite graph capture must make progress");
        let before = job.progress();
        let step = job.advance(1, 1021).unwrap();
        let after = job.progress();
        assert!(after.source.visited_positions - before.source.visited_positions <= 1);
        assert!(after.source.copied_payload_bytes - before.source.copied_payload_bytes <= 1021);
        assert!(after.image.hashed_payload_bytes - before.image.hashed_payload_bytes <= 1021);
        if !changed && after.source.copied_payload_bytes > 1024 {
            Arc::make_mut(source)
                .add_vertex(Vertex::new(99, "later"), "g")
                .unwrap();
            changed = true;
        }
        if step == BackupCaptureStep::Prepared {
            break;
        }
        assert_eq!(step, BackupCaptureStep::Pending);
    }
    assert!(changed);
    let report = job.report().unwrap();
    assert!(report.source.visited_positions > 10);
    assert_eq!(
        report.source.copied_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(
        report.image.hashed_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(report.image.reused_units, u64::from(parent.is_some()));
    let root = job.publish().unwrap();
    drop(job);
    assert_eq!(control.memory().used(), 0);
    root
}
