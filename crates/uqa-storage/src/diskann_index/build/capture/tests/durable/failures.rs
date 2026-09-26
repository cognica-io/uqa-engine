//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    build, capture, options, version, Arc, DiskANNBuildCapture, DiskANNGeneration,
    DiskANNMemorySource, DiskANNOriginReader, DiskANNPageSource, DiskANNRecordKey,
    DiskANNTemporaryBudget, MemoryBudget, StorageReadControl,
};
use crate::diskann_index::{
    format::DiskANNVectorVersion,
    pages::{DiskANNPageVisitor, DiskANNReadCapabilities, DiskANNRecordVisitor},
    DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor,
};
use crate::StorageBackendResult;

struct InvalidSource(u8);

impl DiskANNCanonicalRead for InvalidSource {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        control.check()
    }
    fn dimensions(&self) -> u32 {
        2
    }
    fn next_document_after(
        &self,
        after: Option<u64>,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<u64>> {
        Ok(after.is_none().then_some(0))
    }
    fn origin(
        &self,
        _: u64,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        Ok(Some(version(1)))
    }
    fn visit_document(
        &self,
        _: u64,
        _: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        let raw = if self.0 == 4 {
            [f32::NAN, 0.0]
        } else {
            [1.0, 0.0]
        };
        let _ = visit(0, version(1), &raw);
        if self.0 == 1 {
            let _ = visit(1, version(2), &raw);
        }
        if self.0 == 2 {
            let _ = visit(0, version(1), &raw);
        }
        Ok((self.0 != 3).then(|| version(if self.0 == 0 { 2 } else { 1 })))
    }
}

#[test]
fn origin_capture_rejects_inconsistent_sources_and_suppressed_consumer_errors() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(8192);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    for fault in 0..5 {
        assert!(
            DiskANNBuildCapture::capture(
                DiskANNGeneration::new([11; 16], 1, 2, 3).unwrap(),
                InvalidSource(fault),
                directory.path(),
                &temporary,
                &control
            )
            .is_err(),
            "fault {fault}"
        );
        assert_eq!(temporary.used(), 0);
        assert_eq!(control.memory().used(), 0);
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .is_none());
    }
}

struct FaultSource {
    source: Arc<DiskANNMemorySource>,
    fault: u8,
}

impl DiskANNPageSource for FaultSource {
    fn generation(&self) -> DiskANNGeneration {
        self.source.generation()
    }
    fn capabilities(&self) -> DiskANNReadCapabilities {
        self.source.capabilities()
    }
    fn read_graph_pages(
        &self,
        ids: &[u64],
        control: &StorageReadControl,
        visit: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.source.read_graph_pages(ids, control, visit)
    }
    fn read_record(
        &self,
        key: DiskANNRecordKey,
        maximum: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if key != DiskANNRecordKey::Origins(64) {
            return self.source.read_record(key, maximum, control, visit);
        }
        if self.fault == 0 {
            return Ok(());
        }
        self.source
            .read_record(key, maximum, control, &mut |bytes| {
                if self.fault == 1 {
                    let _ = visit(bytes);
                    let _ = visit(bytes);
                } else {
                    let mut corrupt = bytes.to_vec();
                    *corrupt.last_mut().unwrap() ^= 1;
                    let _ = visit(&corrupt);
                }
                Ok(())
            })
    }
}

#[test]
fn origin_open_never_treats_missing_or_corrupt_batches_as_negative_membership() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(64 << 10);
    let physical = MemoryBudget::new(1 << 20);
    let captured = capture(directory.path(), &temporary, &control, 130, true);
    let (manifest, sink) = build(&captured, directory.path(), &physical);
    let source = Arc::new(sink.finish(manifest, &control).unwrap());
    for fault in 0..3 {
        let damaged = Arc::new(FaultSource {
            source: source.clone(),
            fault,
        });
        assert!(DiskANNOriginReader::open(damaged, options().max_record_bytes, &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    drop((captured, source));
    assert_eq!(temporary.used(), 0);
    assert_eq!(physical.used(), 0);
}
