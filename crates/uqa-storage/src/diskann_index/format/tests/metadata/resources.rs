//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn metadata_admission_failures_release_buffers_and_every_codec_honors_cancellation() {
    let source = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&source);
    let entries = entries(&source);
    let manifest = bundle.manifest.encode(&source).unwrap();
    let retained = source.memory().used();
    for (kind, size) in [
        (0, manifest.len()),
        (1, bundle.book_bytes.len()),
        (2, bundle.code_batch.len()),
        (3, bundle.side_batch.len()),
    ] {
        for limit in [0, size - 1, size] {
            let control = StorageReadControl::with_limit(limit);
            let result = match kind {
                0 => bundle.manifest.encode(&control),
                1 => encode_codebook(generation(), &bundle.book, &control).map(|(bytes, _)| bytes),
                2 => bundle.identity.encode_codes(0, &bundle.codes, &control),
                _ => bundle.side_layout.encode(0, &entries, &control),
            };
            if limit == size {
                assert_eq!(result.as_ref().unwrap().len(), size);
                assert_eq!(control.memory().used(), size);
            } else {
                assert!(matches!(result, Err(StorageBackendError::Memory(_))));
            }
            drop(result);
            assert_eq!(control.memory().used(), 0);
        }
    }
    let mut rejected = 0;
    let mut passed = 0;
    for limit in 0..128 {
        let control = StorageReadControl::with_limit(limit);
        match decode_codebook(&bundle.manifest, &bundle.book_bytes, &control) {
            Ok(book) => {
                passed += 1;
                drop(book);
            }
            Err(StorageBackendError::Memory(_)) => rejected += 1,
            Err(error) => panic!("unexpected error {error}"),
        }
        assert_eq!(control.memory().used(), 0);
    }
    assert!(rejected > 0 && passed > 0);
    let control = StorageReadControl::with_limit(0);
    control.cancellation().cancel();
    let results = [
        bundle.manifest.encode(&control).map(|_| ()),
        DiskANNManifest::decode(generation(), &manifest, &control).map(|_| ()),
        encode_codebook(generation(), &bundle.book, &control).map(|_| ()),
        decode_codebook(&bundle.manifest, &bundle.book_bytes, &control).map(|_| ()),
        bundle
            .identity
            .encode_codes(0, &bundle.codes, &control)
            .map(|_| ()),
        bundle
            .identity
            .decode_codes(0, &bundle.code_batch, &control)
            .map(|_| ()),
        bundle.side_layout.encode(0, &entries, &control).map(|_| ()),
        bundle
            .side_layout
            .decode(0, &bundle.side_batch, &control)
            .map(|_| ()),
        DiskANNSideEntry::from_raw(5, 200, 0, version(), &sides()[0], &control).map(|_| ()),
        artifact_digest(&bundle.code_batch, &control).map(|_| ()),
    ];
    for result in results {
        assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    }
    assert_eq!(source.memory().used(), retained);
}

#[test]
fn a_restored_codebook_retains_its_allowance_without_old_query_cancellation() {
    let source = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&source);
    let control = StorageReadControl::with_limit(80);
    let (book, identity) = decode_codebook(&bundle.manifest, &bundle.book_bytes, &control).unwrap();
    assert_eq!(control.memory().used(), 80);
    assert_eq!(identity, bundle.identity);
    control.cancellation().cancel();
    drop(bundle);
    assert_eq!(source.memory().used(), 0);
    let query = StorageReadControl::with_limit(4096);
    let lookup = book
        .lookup(&navigation(&rows()[0], &query), &query)
        .unwrap();
    let charged = query.memory().used();
    assert!(charged > 0);
    drop(book);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(query.memory().used(), charged);
    assert_eq!(lookup.estimate(&[1, 0], &query).unwrap().get(), 9.0 / 16.0);
    drop(lookup);
    assert_eq!(query.memory().used(), 0);
}
