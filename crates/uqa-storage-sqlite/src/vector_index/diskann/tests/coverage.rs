//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::diskann_index::{
    build::{DiskANNBuildCapture, DiskANNBuildInput, DiskANNTemporaryBudget},
    format::{DiskANNArtifactDigests, DiskANNGeneration, DiskANNManifest, DiskANNManifestInput},
};
use uqa_storage::vector_index::DiskANNIndexParams;

type Capture = DiskANNBuildCapture<RetainedSQLiteDiskANNCanonical>;

mod durable;

#[test]
fn native_diskann_build_coverage_keeps_committed_and_undone_sources_in_all_file_modes() {
    let directory = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let source_control = StorageReadControl::with_limit(1 << 22);
        let build = StorageReadControl::with_limit(64 << 10);
        let query = StorageReadControl::with_limit(8192);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let (committed, private, [original, empty, late, early, undone]) = {
            let connection = open(&directory.path().join(format!("coverage-{mode}.db")), mode);
            capture_pair(
                &connection,
                scratch.path(),
                &temporary,
                &source_control,
                &build,
            )
        };
        let metadata = manifest(committed.input());
        let coverage = committed.finish(&metadata, &query).unwrap();
        for (document, version, expected) in [
            (0, original, true),
            (5, empty, true),
            (7, late, false),
            (9, early, true),
            (0, undone, false),
            (99, original, false),
        ] {
            assert_eq!(
                coverage
                    .contains(DiskANNChangeIdentity::new(document, version), &query)
                    .unwrap(),
                expected
            );
        }
        let metadata = manifest(private.input());
        let private_coverage = private.finish(&metadata, &query).unwrap();
        assert!(private_coverage
            .contains(DiskANNChangeIdentity::new(0, undone), &query)
            .unwrap());
        assert!(!private_coverage
            .contains(DiskANNChangeIdentity::new(0, original), &query)
            .unwrap());
        assert!(private_coverage
            .contains(DiskANNChangeIdentity::new(7, late), &query)
            .unwrap());
        assert_eq!(temporary.used(), 0);
        assert_eq!(query.memory().used(), 0);
        assert!(std::fs::read_dir(scratch.path()).unwrap().next().is_none());
        source_control.cancellation().cancel();
        assert!(coverage
            .contains(DiskANNChangeIdentity::new(99, original), &query)
            .is_err());
    }
}

fn capture_pair(
    connection: &ManagedConnection,
    directory: &Path,
    temporary: &DiskANNTemporaryBudget,
    source_control: &StorageReadControl,
    build: &StorageReadControl,
) -> (Capture, Capture, [DiskANNVectorVersion; 5]) {
    let source = canonical(connection, "docs", "vector", 2);
    let original = source
        .replace(0, &[vec![3.0, 4.0]], source_control)
        .unwrap();
    let empty = source.replace(5, &[], source_control).unwrap();
    let peer = connection.new_session();
    connection.begin_transaction().unwrap();
    let late = source
        .replace(7, &[vec![1.0, 0.0]], source_control)
        .unwrap();
    let peer_source = canonical(&peer, "docs", "vector", 2);
    let early = peer_source
        .replace(9, &[vec![0.0, 0.0]], source_control)
        .unwrap();
    assert!(late.writer().allocation() < early.writer().allocation());
    let committed = DiskANNBuildCapture::capture(
        DiskANNGeneration::new([41; 16], 1, 2, 3).unwrap(),
        peer_source.retain(source_control).unwrap(),
        directory,
        temporary,
        build,
    )
    .unwrap();
    connection.commit_transaction().unwrap();
    connection.begin_transaction().unwrap();
    connection.savepoint("coverage").unwrap();
    let undone = source.replace(0, &[], source_control).unwrap();
    let private = DiskANNBuildCapture::capture(
        DiskANNGeneration::new([41; 16], 1, 2, 4).unwrap(),
        source.retain(source_control).unwrap(),
        directory,
        temporary,
        build,
    )
    .unwrap();
    connection.rollback_to_savepoint("coverage").unwrap();
    connection.rollback_transaction().unwrap();
    (committed, private, [original, empty, late, early, undone])
}

// The shared conformance suite constructs and seals physical artifacts; this fixture isolates binding to a closed native source.
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
