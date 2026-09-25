//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::build::{
    DiskANNMergeOptions, DiskANNMergeSummary, DiskANNPartitionOptions, DiskANNPartitionSummary,
};

fn provenance(bundle: &Bundle, training: PQTrainingOptions) -> DiskANNBuildProvenance {
    let summary = DiskANNMergeSummary {
        work_order_revision: 1,
        partitions: DiskANNPartitionSummary {
            work_order_revision: 1,
            options: DiskANNPartitionOptions {
                max_partition_points: 4,
                max_depth: 4,
                coarse_training: PQTrainingOptions {
                    max_samples: 64,
                    max_iterations: 3,
                    max_centroids: 4,
                    seed: 42,
                },
            },
            parameters: bundle.manifest.input().parameters,
            coverage: bundle.manifest.input().coverage,
            partitions: 3,
            memberships: 10,
            edges: 16,
            maximum_depth: 0,
            maximum_partition_points: 4,
            assignment_digest: [0x55; 32],
        },
        options: DiskANNMergeOptions {
            sort_buffer_records: 4,
        },
        merge_passes: 2,
        edges: 8,
        adjacency_digest: [0x66; 32],
    };
    DiskANNBuildProvenance::from_merge(&summary, 8, training, 3, 2).unwrap()
}

#[test]
fn versioned_build_manifest_matches_independent_bytes_and_retains_legacy_records() {
    let control = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&control);
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/diskann/provenance.json"
    ))
    .unwrap();
    let legacy = bundle.manifest.encode(&control).unwrap();
    assert_eq!(
        hex(&artifact_digest(&legacy, &control).unwrap()),
        oracle()["manifest_sha256"]
    );
    assert!(DiskANNManifest::decode(generation(), &legacy, &control)
        .unwrap()
        .build_provenance()
        .is_none());
    let build = provenance(&bundle, bundle.book.training().options);
    let manifest = bundle.manifest.with_build_provenance(build).unwrap();
    let encoded = manifest.encode(&control).unwrap();
    assert_eq!(encoded.len(), DiskANNManifest::MAX_ENCODED_BYTES);
    assert_eq!(hex(&encoded[384..]), expected["provenance_hex"]);
    assert_eq!(hex(&encoded[64..96]), expected["manifest_checksum"]);
    assert_eq!(
        hex(&artifact_digest(&encoded, &control).unwrap()),
        expected["manifest_sha256"]
    );
    assert_eq!(
        DiskANNManifest::decode(generation(), &encoded, &control).unwrap(),
        manifest
    );
    assert_eq!(manifest.build_provenance(), Some(&build));
    assert_eq!(build.entry_sample_points(), 8);
    assert_eq!(build.code_batch_nodes(), 3);
    assert_eq!(build.side_batch_entries(), 2);
    decode_codebook(&manifest, &bundle.book_bytes, &control).unwrap();
    let tiny = StorageReadControl::with_limit(encoded.len() - 1);
    assert!(matches!(
        manifest.encode(&tiny),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    drop((bundle, legacy, encoded));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn rechecksummed_build_metadata_cannot_change_revisions_shapes_or_effective_training() {
    let control = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&control);
    let manifest = bundle
        .manifest
        .with_build_provenance(provenance(&bundle, bundle.book.training().options))
        .unwrap();
    let valid = manifest.encode(&control).unwrap();
    for (word, value) in [
        (0, 2),
        (1, 2),
        (2, 1),
        (3, 256),
        (4, u64::MAX),
        (5, 0),
        (6, 2),
        (8, 0),
        (9, 7),
        (10, 21),
        (11, 5),
        (12, 9),
        (13, 2),
        (14, 0),
        (15, 3),
        (16, 7),
        (17, 0),
        (18, 257),
        (19, 0),
        (20, 43),
        (21, 0),
        (22, 0),
        (23, 7),
    ] {
        let mut corrupt = valid.to_vec();
        corrupt[384 + word * 8..392 + word * 8].copy_from_slice(&value.to_le_bytes());
        reseal(&mut corrupt);
        assert!(
            DiskANNManifest::decode(generation(), &corrupt, &control).is_err(),
            "word {word}"
        );
    }
    for revision in [1_u32, 3] {
        let mut corrupt = valid.to_vec();
        corrupt[8..12].copy_from_slice(&revision.to_le_bytes());
        reseal(&mut corrupt);
        assert!(DiskANNManifest::decode(generation(), &corrupt, &control).is_err());
    }
    assert!(DiskANNManifest::decode(generation(), &valid[..valid.len() - 1], &control).is_err());
    let mut different = bundle.book.training().options;
    different.max_samples += 1;
    let mismatch = bundle
        .manifest
        .with_build_provenance(provenance(&bundle, different))
        .unwrap();
    assert!(decode_codebook(&mismatch, &bundle.book_bytes, &control).is_err());
}
