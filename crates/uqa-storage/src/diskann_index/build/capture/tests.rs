//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::{
    format::{DiskANNArtifactDigests, DiskANNManifestInput, DiskANNVectorVersion},
    DiskANNCanonicalVectorVisitor,
};
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::vector_index::DiskANNIndexParams;
use uqa_core::DocId;

mod durable;

struct Source {
    revision: u64,
    documents: u64,
    emit_vectors: bool,
    control: StorageReadControl,
}

fn version(revision: u64) -> DiskANNVectorVersion {
    DiskANNVectorVersion::new(
        StorageTransactionId::new(DatabaseId::from_bytes([23; 16]), 7).unwrap(),
        revision,
    )
    .unwrap()
}

impl DiskANNCanonicalRead for Source {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()
    }
    fn dimensions(&self) -> u32 {
        2
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.check_control(control)?;
        Ok((0..self.documents).find(|&doc| after.is_none_or(|after| doc > after)))
    }
    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        Ok((document < self.documents).then(|| version(self.revision + document)))
    }
    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        let selected = self.origin(document, control)?;
        if let Some(origin) = selected.filter(|_| self.emit_vectors) {
            match document {
                0 => visit(0, origin, &[3.0, 4.0])?,
                2 => visit(0, origin, &[-0.0, 0.0])?,
                _ => {}
            }
        }
        Ok(selected)
    }
}

fn capture(
    directory: &Path,
    temporary: &DiskANNTemporaryBudget,
    source: &StorageReadControl,
    build: &StorageReadControl,
    revision: u64,
) -> DiskANNBuildCapture<Source> {
    DiskANNBuildCapture::capture(
        DiskANNGeneration::new([11; 16], 1, 2, 3).unwrap(),
        Source {
            revision,
            documents: 3,
            emit_vectors: true,
            control: source.clone(),
        },
        directory,
        temporary,
        build,
    )
    .unwrap()
}

// This fixture tests membership binding only; actual-provider conformance also constructs and physically seals a generation.
fn manifest(input: &DiskANNBuildInput) -> DiskANNManifest {
    DiskANNManifest::new(DiskANNManifestInput {
        generation: input.coverage().generation(),
        dimensions: input.dimensions(),
        parameters: DiskANNIndexParams::for_dimensions(input.dimensions()).unwrap(),
        nodes: input.node_count(),
        side_vectors: input.side_count(),
        entry_node: (input.node_count() > 0).then_some(0),
        coverage: input.coverage(),
        artifacts: DiskANNArtifactDigests::empty(),
    })
    .unwrap()
}

#[test]
fn completed_capture_retains_empty_tensor_membership_and_releases_temporary_input() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(8192);
    let captured = capture(directory.path(), &temporary, &control, &control, 1);
    let metadata = manifest(captured.input());
    let coverage = captured.finish(&metadata, &control).unwrap();
    assert_eq!(coverage.fingerprint(), metadata.input().coverage);
    for (document, revision, expected) in [
        (0, 1, true),
        (1, 2, true),
        (2, 3, true),
        (0, 2, false),
        (99, 1, false),
    ] {
        assert_eq!(
            coverage
                .contains(
                    DiskANNChangeIdentity::new(document, version(revision)),
                    &control
                )
                .unwrap(),
            expected
        );
    }
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
    assert!(std::fs::read_dir(directory.path())
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn equal_empty_fingerprints_cannot_substitute_for_actual_source_membership() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(8192);
    let mut fingerprint = None;
    for present in [false, true] {
        let captured = DiskANNBuildCapture::capture(
            DiskANNGeneration::new([11; 16], 1, 2, 3).unwrap(),
            Source {
                revision: 1,
                documents: if present { 3 } else { 0 },
                emit_vectors: false,
                control: control.clone(),
            },
            directory.path(),
            &temporary,
            &control,
        )
        .unwrap();
        let metadata = manifest(captured.input());
        let coverage = captured.finish(&metadata, &control).unwrap();
        assert_eq!(
            *fingerprint.get_or_insert(coverage.fingerprint()),
            coverage.fingerprint()
        );
        assert_eq!(
            coverage
                .contains(DiskANNChangeIdentity::new(0, version(1)), &control)
                .unwrap(),
            present
        );
    }
    assert_eq!(temporary.used(), 0);
}

#[test]
fn captured_membership_rejects_other_origins_and_changed_navigation_classification() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let control = StorageReadControl::with_limit(8192);
    let other = capture(directory.path(), &temporary, &control, &control, 99);
    let other_manifest = manifest(other.input());
    drop(other);
    let captured = capture(directory.path(), &temporary, &control, &control, 1);
    assert!(captured.finish(&other_manifest, &control).is_err());
    let captured = capture(directory.path(), &temporary, &control, &control, 1);
    let mut changed = *manifest(captured.input()).input();
    changed.nodes = 0;
    changed.side_vectors = 2;
    changed.entry_node = None;
    assert!(captured
        .finish(&DiskANNManifest::new(changed).unwrap(), &control)
        .is_err());
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn build_coverage_keeps_source_build_and_invoking_cancellation_after_finish() {
    for cancel in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let controls = std::array::from_fn::<_, 3, _>(|_| StorageReadControl::with_limit(8192));
        let captured = capture(directory.path(), &temporary, &controls[0], &controls[1], 1);
        let metadata = manifest(captured.input());
        let coverage = captured.finish(&metadata, &controls[2]).unwrap();
        controls[cancel].cancellation().cancel();
        for document in [0, 99] {
            assert!(coverage
                .contains(
                    DiskANNChangeIdentity::new(document, version(1)),
                    &controls[2]
                )
                .is_err());
        }
        assert_eq!(temporary.used(), 0);
    }
}

#[test]
fn cancelled_capture_completion_releases_its_source_and_temporary_files() {
    for cancel in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let controls = std::array::from_fn::<_, 3, _>(|_| StorageReadControl::with_limit(8192));
        let captured = capture(directory.path(), &temporary, &controls[0], &controls[1], 1);
        let metadata = manifest(captured.input());
        controls[cancel].cancellation().cancel();
        assert!(captured.finish(&metadata, &controls[2]).is_err());
        assert_eq!(temporary.used(), 0);
        assert_eq!(controls[1].memory().used(), 0);
    }
}
