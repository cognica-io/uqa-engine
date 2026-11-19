//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    document_store::{image::decode_document_image, DocumentMetadata, StoredDocument},
    image_io::ImageReader,
    DocumentStore, MemoryDocumentStore,
};
use uqa_core::{DocId, Value};

// A single owning document unit exercises repository admission; complete Engine images additionally require their catalog, index and resource owners.
struct DocumentUnit<'a> {
    cut: BackupCut,
    reader: ImageReader<'a>,
    key: [u8; 8],
    selected: bool,
    ended: bool,
    statistics: BackupSourceStatistics,
}

impl<'a> DocumentUnit<'a> {
    fn new(source: &'a dyn DocumentStore, id: DocId, control: &'a StorageReadControl) -> Self {
        Self {
            cut: cut(),
            reader: source.image_document_reader(id, control).unwrap().unwrap(),
            key: id.to_be_bytes(),
            selected: false,
            ended: false,
            statistics: BackupSourceStatistics::default(),
        }
    }
}

impl BackupImageSource for DocumentUnit<'_> {
    fn cut(&self) -> BackupCut {
        self.cut
    }
    fn advance(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        assert!(maximum > 0);
        self.statistics.visited_positions += 1;
        if self.selected {
            assert!(self.ended, "cannot skip an incomplete document image");
            Ok(BackupSourceStep::Complete)
        } else {
            self.selected = true;
            Ok(BackupSourceStep::Unit)
        }
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.selected.then_some(BackupSourceUnit {
            address: UnitAddress {
                namespace: NamespaceId::from_bytes([7; 16]).unwrap(),
                generation: 0,
                family: 1,
                key: &self.key,
            },
            kind: UnitKind::Record,
            decoded_bytes: None,
        })
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let bytes = self.reader.read(output)?;
        self.statistics.copied_payload_bytes += bytes as u64;
        self.ended = bytes == 0 && !output.is_empty();
        Ok(bytes)
    }
    fn statistics(&self) -> BackupSourceStatistics {
        self.statistics
    }
}

#[test]
fn memory_document_streams_once_into_full_and_incremental_backups_after_source_loss() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    let repository = repository(directory.path(), &control);
    let mut source = MemoryDocumentStore::new();
    let length = (2 << 20) + 17;
    let tuple = StoredDocument::with_metadata(
        [
            ("payload".into(), Value::Bytes(vec![42; length])),
            ("counter".into(), Value::Int(99)),
        ]
        .into(),
        DocumentMetadata::with_tuple_xmin(1234),
    );
    source.put_stored(7, tuple).unwrap();
    let retention = StorageReadControl::with_limit(8 << 20);
    let snapshot = source.snapshot_with_control(&retention).unwrap();
    source.delete(7).unwrap();
    drop(source);
    let mut roots = Vec::new();
    let mut parent = None;
    for _ in 0..2 {
        let image = DocumentUnit::new(snapshot.as_ref(), 7, &control);
        assert_eq!(image.statistics().copied_payload_bytes, 0);
        let mut job = repository
            .begin_image_backup(image, parent, 16 << 20, 8 << 20, &control)
            .unwrap();
        finish(&mut job, 1021);
        let report = job.report().unwrap();
        assert_eq!(report.inventory.units, 1);
        assert!(report.inventory.decoded_bytes > length as u64);
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
        parent = Some(root.backup());
        roots.push(root);
        drop(job);
        assert_eq!(control.memory().used(), 0);
    }
    drop(snapshot);
    assert_eq!(retention.memory().used(), 0);
    assert_eq!(roots[0].summary(), roots[1].summary());
    repository
        .remove_backup(roots[0].backup(), &control)
        .unwrap();
    let restored = read(&repository, roots[1], &control);
    let destination = StorageReadControl::with_limit(8 << 20);
    let document = decode_document_image(&restored[0].0.data, &destination).unwrap();
    assert_eq!(document.metadata(), DocumentMetadata::with_tuple_xmin(1234));
    assert_eq!(document.fields().get("counter"), Some(&Value::Int(99)));
    let Some(Value::Bytes(bytes)) = document.fields().get("payload") else {
        panic!("restored payload")
    };
    assert_eq!(bytes.len(), length);
    assert!(bytes.iter().all(|&byte| byte == 42));
    drop(document);
    assert_eq!(destination.memory().used(), 0);
    assert!(control.memory().peak() <= 128 << 10);
}
