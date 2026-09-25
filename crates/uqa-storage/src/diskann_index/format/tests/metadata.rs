//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::{
    pq::{PQCodebook, PQTrainer, PQTrainingOptions},
    NavigationInput, NavigationVector,
};
use crate::vector_index::DiskANNIndexParams;

mod corruption;
mod provenance;
mod resources;

fn oracle() -> Value {
    serde_json::from_str(include_str!(
        "../../../../tests/fixtures/diskann/metadata.json"
    ))
    .unwrap()
}

fn rows() -> Vec<Vec<f32>> {
    let data: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/diskann/training.json"
    ))
    .unwrap();
    data["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap() as f32)
                .collect()
        })
        .collect()
}

fn navigation(raw: &[f32], control: &StorageReadControl) -> NavigationVector {
    match NavigationInput::from_raw(raw.len() as u32, raw, control).unwrap() {
        NavigationInput::Navigable(vector) => vector,
        NavigationInput::Exact(_) => panic!("navigable reference"),
    }
}

fn train(seed: u64, control: &StorageReadControl) -> PQCodebook {
    let mut trainer = PQTrainer::new(
        5,
        2,
        PQTrainingOptions {
            max_samples: 5,
            max_centroids: 2,
            seed,
            ..PQTrainingOptions::default()
        },
        control,
    )
    .unwrap();
    for row in rows() {
        trainer.observe(&navigation(&row, control)).unwrap();
    }
    trainer.finish().unwrap()
}

fn sides() -> [[f32; 5]; 2] {
    [[0.0, 0.0, 0.0, 0.0, -0.0], [f32::MAX, 0.0, 0.0, 0.0, 0.0]]
}

fn entries(control: &StorageReadControl) -> [DiskANNSideEntry; 2] {
    std::array::from_fn(|i| {
        DiskANNSideEntry::from_raw(5, 200, i as u32, version(), &sides()[i], control).unwrap()
    })
}

struct Bundle {
    manifest: DiskANNManifest,
    book: PQCodebook,
    book_bytes: BudgetedVec<u8>,
    identity: DiskANNQuantizationIdentity,
    codes: Vec<u8>,
    code_batch: BudgetedVec<u8>,
    side_layout: DiskANNSideLayout,
    side_batch: BudgetedVec<u8>,
}

fn bundle(control: &StorageReadControl) -> Bundle {
    let book = train(42, control);
    let (book_bytes, identity) = encode_codebook(generation(), &book, control).unwrap();
    let mut codes = Vec::new();
    let mut coverage = DiskANNCoverageBuilder::new(generation(), 5).unwrap();
    let layout = DiskANNNodeLayout::new(5, 2, 8).unwrap();
    let mut nodes = Vec::new();
    for (id, raw) in rows().iter().enumerate() {
        codes.extend_from_slice(&book.encode(&navigation(raw, control), control).unwrap());
        coverage
            .push(100 + id as u64, 0, version(), raw, control)
            .unwrap();
        let node = DiskANNNodeInput {
            node_id: id as u64,
            doc_id: 100 + id as u64,
            ordinal: 0,
            version: version(),
            vector: raw,
            neighbors: &[(id as u64 + 1) % 8],
        };
        nodes.extend_from_slice(&layout.encode_node(&node, control).unwrap());
    }
    let graph = encode_page(generation(), layout, 0, &nodes, control).unwrap();
    for (ordinal, raw) in sides().iter().enumerate() {
        coverage
            .push(200, ordinal as u32, version(), raw, control)
            .unwrap();
    }
    let side_layout = DiskANNSideLayout::new(generation(), 5, 2).unwrap();
    let side_batch = side_layout.encode(0, &entries(control), control).unwrap();
    let side = side_layout.decode(0, &side_batch, control).unwrap();
    let parameters = DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 4,
        search_list_size: 8,
        beam_width: 2,
        pq_bytes: 2,
        ..DiskANNIndexParams::for_dimensions(5).unwrap()
    };
    let manifest = DiskANNManifest::new(DiskANNManifestInput {
        generation: generation(),
        dimensions: 5,
        parameters,
        nodes: 8,
        side_vectors: 2,
        entry_node: Some(0),
        coverage: coverage.finish(),
        artifacts: DiskANNArtifactDigests {
            codebook: identity.codebook_digest(),
            codes: artifact_digest(&codes, control).unwrap(),
            side: artifact_digest(side.bytes(), control).unwrap(),
            graph: artifact_digest(&graph[112..144], control).unwrap(),
        },
    })
    .unwrap();
    let code_batch = identity.encode_codes(0, &codes, control).unwrap();
    Bundle {
        manifest,
        book,
        book_bytes,
        identity,
        codes,
        code_batch,
        side_layout,
        side_batch,
    }
}

fn reseal(bytes: &mut [u8]) {
    let mut hash = Sha256::new();
    hash.update(&bytes[..64]);
    hash.update(&bytes[96..]);
    bytes[64..96].copy_from_slice(&hash.finalize());
}

#[test]
fn generation_metadata_matches_independent_bytes_and_preserves_quantization() {
    let control = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&control);
    let expected = oracle();
    let bytes = bundle.manifest.encode(&control).unwrap();
    for (key, actual) in [
        (
            "manifest_sha256",
            artifact_digest(&bytes, &control).unwrap(),
        ),
        ("codebook_sha256", bundle.identity.codebook_digest()),
        ("coverage_sha256", bundle.manifest.input().coverage.digest()),
        ("graph_sha256", bundle.manifest.input().artifacts.graph),
        ("codes_sha256", bundle.manifest.input().artifacts.codes),
        ("side_sha256", bundle.manifest.input().artifacts.side),
        (
            "code_batch_sha256",
            artifact_digest(&bundle.code_batch, &control).unwrap(),
        ),
        (
            "side_batch_sha256",
            artifact_digest(&bundle.side_batch, &control).unwrap(),
        ),
    ] {
        assert_eq!(hex(&actual), expected[key].as_str().unwrap(), "{key}");
    }
    assert_eq!(
        hex(&bundle.book_bytes[..160]),
        expected["codebook_header_hex"].as_str().unwrap()
    );
    let reader = StorageReadControl::with_limit(0);
    let manifest = DiskANNManifest::decode(generation(), &bytes, &reader).unwrap();
    assert_eq!(manifest, bundle.manifest);
    let (book, identity) = decode_codebook(&manifest, &bundle.book_bytes, &control).unwrap();
    assert_eq!(identity, bundle.identity);
    assert_eq!(book.training(), bundle.book.training());
    let (copy, _) = encode_codebook(generation(), &book, &control).unwrap();
    assert_eq!(&*copy, &*bundle.book_bytes);
    let codes = identity
        .decode_codes(0, &bundle.code_batch, &reader)
        .unwrap();
    assert_eq!(
        (
            codes.first_node(),
            codes.node_count(),
            identity.pq_bytes(),
            identity.node_count()
        ),
        (0, 8, 2, 8)
    );
    let lookup = book
        .lookup(&navigation(&rows()[0], &control), &control)
        .unwrap();
    for (node, expected) in [
        9.0 / 16.0,
        9.0 / 16.0,
        9.0 / 16.0,
        2.0,
        9.0 / 16.0,
        9.0 / 16.0,
        9.0 / 16.0,
        25.0 / 16.0,
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            lookup
                .estimate(codes.code(node as u64).unwrap(), &reader)
                .unwrap()
                .get(),
            *expected
        );
    }
    assert!(codes.code(8).is_none());
    assert!(codes.code(u64::MAX).is_none());
    let side = bundle
        .side_layout
        .decode(0, &bundle.side_batch, &reader)
        .unwrap();
    assert_eq!((side.first_record(), side.record_count()), (0, 2));
    assert_eq!(
        hex(side.bytes()),
        expected["side_entry_hex"].as_str().unwrap()
    );
    for (index, entry) in entries(&control).iter().enumerate() {
        assert_eq!(side.entry(index), Some(*entry));
    }
    assert!(side.entry(2).is_none());
    assert!(side.entry(usize::MAX).is_none());
    assert_eq!(reader.memory().used(), 0);
}

#[test]
fn coverage_preserves_raw_bits_origins_and_an_atomic_ordered_prefix() {
    let control = StorageReadControl::with_limit(0);
    let row = [1.0, 0.0, 0.0, 0.0, 0.0];
    let mut baseline = DiskANNCoverageBuilder::new(generation(), 5).unwrap();
    baseline.push(100, 0, version(), &row, &control).unwrap();
    baseline.push(100, 1, version(), &row, &control).unwrap();
    let mut actual = DiskANNCoverageBuilder::new(generation(), 5).unwrap();
    assert!(actual.push(100, 1, version(), &row, &control).is_err());
    actual.push(100, 0, version(), &row, &control).unwrap();
    for (doc, ordinal) in [(100, 0), (100, 2), (99, 0), (101, 1)] {
        assert!(actual
            .push(doc, ordinal, version(), &row, &control)
            .is_err());
    }
    assert!(actual
        .push(100, 1, version(), &[f32::NAN; 5], &control)
        .is_err());
    let cancelled = StorageReadControl::with_limit(0);
    cancelled.cancellation().cancel();
    assert!(actual.push(100, 1, version(), &row, &cancelled).is_err());
    actual.push(100, 1, version(), &row, &control).unwrap();
    assert_eq!(actual.finish(), baseline.finish());
    let mut digests = Vec::new();
    for (raw, origin) in [
        (row, version()),
        ([1.0, -0.0, 0.0, 0.0, 0.0], version()),
        (
            row,
            DiskANNVectorVersion::new(version().writer(), 10).unwrap(),
        ),
    ] {
        let mut selected = DiskANNCoverageBuilder::new(generation(), 5).unwrap();
        selected.push(100, 0, origin, &raw, &control).unwrap();
        let selected = selected.finish();
        assert_eq!(selected.vector_count(), 1);
        digests.push(selected.digest());
    }
    assert!(digests[0] != digests[1] && digests[0] != digests[2] && digests[1] != digests[2]);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn empty_and_all_side_generations_have_no_entry_or_fake_codebook() {
    let control = StorageReadControl::with_limit(4096);
    for count in [0, 2] {
        let mut coverage = DiskANNCoverageBuilder::new(generation(), 5).unwrap();
        for (ordinal, raw) in sides().iter().enumerate().take(count) {
            coverage
                .push(200, ordinal as u32, version(), raw, &control)
                .unwrap();
        }
        let mut artifacts = DiskANNArtifactDigests::empty();
        if count != 0 {
            let layout = DiskANNSideLayout::new(generation(), 5, count as u64).unwrap();
            let bytes = layout
                .encode(0, &entries(&control)[..count], &control)
                .unwrap();
            artifacts.side = artifact_digest(
                layout.decode(0, &bytes, &control).unwrap().bytes(),
                &control,
            )
            .unwrap();
        }
        let input = DiskANNManifestInput {
            generation: generation(),
            dimensions: 5,
            parameters: DiskANNIndexParams::for_dimensions(5).unwrap(),
            nodes: 0,
            side_vectors: count as u64,
            entry_node: None,
            coverage: coverage.finish(),
            artifacts,
        };
        let manifest = DiskANNManifest::new(input).unwrap();
        assert_eq!(manifest.layout().page_count(), 0);
        let bytes = manifest.encode(&control).unwrap();
        assert_eq!(
            DiskANNManifest::decode(generation(), &bytes, &control).unwrap(),
            manifest
        );
        assert!(decode_codebook(&manifest, &[], &control).is_err());
        assert!(DiskANNManifest::new(DiskANNManifestInput {
            entry_node: Some(0),
            ..input
        })
        .is_err());
        assert!(DiskANNManifest::new(DiskANNManifestInput {
            side_vectors: 1,
            ..input
        })
        .is_err());
        let mut wrong = input;
        wrong.artifacts.codebook = [1; 32];
        assert!(DiskANNManifest::new(wrong).is_err());
    }
    assert!(DiskANNCoverageBuilder::new(generation(), 0).is_err());
    assert!(DiskANNSideLayout::new(generation(), 0, 0).is_err());
}

#[test]
fn independently_addressed_code_and_side_batches_preserve_ordered_streams() {
    let control = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&control);
    let first = bundle
        .identity
        .encode_codes(0, &bundle.codes[..6], &control)
        .unwrap();
    let last = bundle
        .identity
        .encode_codes(3, &bundle.codes[6..], &control)
        .unwrap();
    let first = bundle.identity.decode_codes(0, &first, &control).unwrap();
    let last = bundle.identity.decode_codes(3, &last, &control).unwrap();
    assert_eq!([first.bytes(), last.bytes()].concat(), bundle.codes);
    assert!(last.code(2).is_none());
    assert_eq!(last.code(3), Some(&bundle.codes[6..8]));
    let entries = entries(&control);
    let mut joined = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let bytes = bundle
            .side_layout
            .encode(index as u64, &[*entry], &control)
            .unwrap();
        let batch = bundle
            .side_layout
            .decode(index as u64, &bytes, &control)
            .unwrap();
        joined.extend_from_slice(batch.bytes());
    }
    assert_eq!(
        artifact_digest(&joined, &control).unwrap(),
        bundle.manifest.input().artifacts.side
    );
    assert!(bundle
        .identity
        .decode_codes(1, &bundle.code_batch, &control)
        .is_err());
    assert!(bundle
        .side_layout
        .decode(1, &bundle.side_batch, &control)
        .is_err());
    assert!(bundle
        .identity
        .encode_codes(u64::MAX, &bundle.codes, &control)
        .is_err());
    assert!(bundle.identity.encode_codes(0, &[], &control).is_err());
    assert!(bundle
        .side_layout
        .encode(u64::MAX, &entries, &control)
        .is_err());
    assert!(bundle.side_layout.encode(0, &[], &control).is_err());
}
