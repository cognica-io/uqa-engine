//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::pages::DiskANNArtifactSealer;

fn reseal(bytes: &mut [u8]) {
    let mut hash = Sha256::new();
    hash.update(&bytes[..64]);
    hash.update(&bytes[96..]);
    bytes[64..96].copy_from_slice(&hash.finalize());
}

fn replay(
    source: &dyn DiskANNPageSource,
    manifest: &DiskANNManifest,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut seal = DiskANNArtifactSealer::new(*manifest, control)?;
    for key in [
        DiskANNRecordKey::Codebook,
        DiskANNRecordKey::Codes(0),
        DiskANNRecordKey::Codes(3),
        DiskANNRecordKey::Side(0),
        DiskANNRecordKey::Side(2),
    ] {
        source.read_record(
            key,
            options().max_record_bytes,
            control,
            &mut |bytes| match key {
                DiskANNRecordKey::Codebook => seal.codebook(bytes),
                DiskANNRecordKey::Codes(first) => seal.code_batch(first, bytes),
                DiskANNRecordKey::Side(first) => seal.side_batch(first, bytes),
                DiskANNRecordKey::Origins(first) => seal.origin_batch(first, bytes),
                DiskANNRecordKey::Manifest => unreachable!(),
            },
        )?;
    }
    source.read_graph_pages(&[0], control, &mut |id, bytes| seal.graph_page(id, bytes))?;
    seal.finish().map(|_| ())
}

#[test]
fn physical_seal_rejects_rechecksummed_fingerprints_and_declared_batch_mismatches() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let physical = MemoryBudget::new(64 << 10);
    let input = capture(directory.path(), &temporary, &control, 2, 4, 3);
    let graph = merged(&input, directory.path());
    let mut sink = DiskANNMemoryBuilder::new(generation(), &physical);
    let manifest = input
        .write_generation(&graph, options(), &mut sink)
        .unwrap();
    let source = sink.finish(manifest, &control).unwrap();
    replay(&source, &manifest, &control).unwrap();
    let valid = manifest.encode(&control).unwrap();
    for (offset, value) in [(384 + 224, 7_u64), (384 + 21 * 8, 2), (384 + 22 * 8, 1)] {
        let mut corrupt = valid.to_vec();
        corrupt[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        reseal(&mut corrupt);
        let corrupt = DiskANNManifest::decode(generation(), &corrupt, &control).unwrap();
        assert!(
            replay(&source, &corrupt, &control).is_err(),
            "offset {offset}"
        );
    }
    let legacy = DiskANNManifest::new(*manifest.input()).unwrap();
    replay(&source, &legacy, &control).unwrap();
    drop((source, input, graph, valid));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(temporary.used(), 0);
    assert_eq!(physical.used(), 0);
}

#[test]
fn a_well_formed_page_without_the_required_global_successor_cannot_seal() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let physical = MemoryBudget::new(64 << 10);
    let input = capture(directory.path(), &temporary, &control, 2, 3, 0);
    let graph = merged(&input, directory.path());
    let mut sink = DiskANNMemoryBuilder::new(generation(), &physical);
    let manifest = input
        .write_generation(&graph, options(), &mut sink)
        .unwrap();
    let layout = manifest.layout();
    let mut payload = Vec::new();
    for id in 0..3 {
        let record = input.read_node(id).unwrap();
        payload.extend_from_slice(
            &layout
                .encode_node(
                    &DiskANNNodeInput {
                        node_id: id,
                        doc_id: record.doc_id(),
                        ordinal: record.ordinal(),
                        version: record.version(),
                        vector: record.raw(),
                        neighbors: &[(id + 2) % 3],
                    },
                    &control,
                )
                .unwrap(),
        );
    }
    let page = encode_page(generation(), layout, 0, &payload, &control).unwrap();
    let mut seal = DiskANNArtifactSealer::new(manifest, &control).unwrap();
    let error = seal.graph_page(0, &page).unwrap_err();
    assert!(error.to_string().contains("global successor"));
    assert!(seal.finish().is_err());
    drop((sink, input, graph, page));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(temporary.used(), 0);
    assert_eq!(physical.used(), 0);
}
