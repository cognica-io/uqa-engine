//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{document_store::Document, DocumentStore, MemoryDocumentStore, ReadOnlySnapshot};
use std::collections::BTreeMap;
use uqa_core::{DatumValue, RecordValue};

#[test]
fn document_image_headers_keep_typed_cancellation_between_io_attempts() {
    struct Interrupted {
        token: uqa_core::CancellationToken,
        calls: usize,
        partial: bool,
    }
    impl Read for Interrupted {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                self.token.cancel();
                if self.partial {
                    output[0] = b'U';
                    return Ok(1);
                }
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            output.fill(0);
            Ok(output.len())
        }
    }
    impl Write for Interrupted {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                self.token.cancel();
                if self.partial {
                    return Ok(1);
                }
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for partial in [false, true] {
        let control = StorageReadControl::with_limit(1024);
        let mut io = Interrupted {
            token: control.cancellation().clone(),
            calls: 0,
            partial,
        };
        assert!(matches!(
            read_document_image(&mut io, &control),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(io.calls, 1);
        control.cancellation().reset();
        io.calls = 0;
        assert!(matches!(
            write(
                std::iter::empty(),
                DocumentMetadata::default(),
                &mut io,
                &control,
            ),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(io.calls, 1);
        assert_eq!(control.memory().used(), 0);
    }
}

fn encode(store: &dyn DocumentStore, id: u64, control: &StorageReadControl) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let written = store
        .write_image_document(id, &mut bytes, control)
        .unwrap()?;
    assert_eq!(written, bytes.len() as u64);
    Some(bytes)
}

#[test]
fn memory_document_images_preserve_captured_rows_metadata_and_future_writes() {
    let control = StorageReadControl::with_limit(1 << 20);
    let expected = StoredDocument::with_metadata(
        Document::from([
            ("xmin".into(), Value::Str("user field".into())),
            ("\0uqa.system.xmin".into(), Value::Int(-9)),
            (
                "tag".into(),
                Value::Map(BTreeMap::from([(
                    "$uqa_type".into(),
                    Value::Str("void".into()),
                )])),
            ),
            (
                "lazy".into(),
                Value::Datum(DatumValue::new(25, u32::MAX, vec![0, 255])),
            ),
            (
                "record".into(),
                Value::Record(RecordValue::from_parts(
                    vec![("same".into(), Value::Void), ("same".into(), Value::Null)],
                    Some(400),
                )),
            ),
        ]),
        DocumentMetadata::with_tuple_xmin(u32::MAX),
    );
    let mut source = MemoryDocumentStore::new();
    source.put_stored(7, expected.clone()).unwrap();
    source.put_stored(1, StoredDocument::default()).unwrap();
    let retained = ReadOnlySnapshot::new(source.snapshot().unwrap());
    source.delete(7).unwrap();
    source
        .put_stored(
            7,
            StoredDocument::new(Document::from([("new layout".into(), Value::Int(19))])),
        )
        .unwrap();
    source.clear().unwrap();
    drop(source);
    let bytes = encode(&retained, 7, &control).unwrap();
    assert_eq!(&bytes[..13], b"UQADOC01\x01\xff\xff\xff\xff");
    let decoded = decode_document_image(&bytes, &control).unwrap();
    assert_eq!(&*decoded, &expected);
    let (document, reservation) = decoded.into_parts();
    let mut restored = MemoryDocumentStore::new();
    restored.put_stored(7, document).unwrap();
    // The explicit ordinary store now owns the imported row; its caller releases staging admission.
    drop(reservation);
    assert_eq!(restored.get_stored(7).unwrap(), Some(expected));
    restored
        .put(7, Document::from([("later".into(), Value::Int(42))]))
        .unwrap();
    assert_eq!(
        restored.get_metadata(7).unwrap(),
        Some(DocumentMetadata::with_tuple_xmin(u32::MAX))
    );
    let empty = encode(&retained, 1, &control).unwrap();
    assert_eq!(&empty[..9], b"UQADOC01\0");
    assert_eq!(
        *decode_document_image(&empty, &control).unwrap(),
        StoredDocument::default()
    );
    assert!(encode(&retained, 999, &control).is_none());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_document_image_export_streams_large_borrowed_values() {
    let mut store = MemoryDocumentStore::new();
    store
        .put(
            1,
            Document::from([
                ("bytes".into(), Value::Bytes(vec![7; 2 << 20])),
                ("text".into(), Value::Str("a".repeat(2 << 20))),
            ]),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(512);
    let retained = store.snapshot().unwrap();
    drop(store);
    let allocations = allocation_counter::measure(|| {
        let size = retained
            .write_image_document(1, &mut std::io::sink(), &control)
            .unwrap()
            .unwrap();
        assert!(size > 4 << 20);
    });
    assert!(allocations.bytes_total < 2048, "{allocations:?}");
    assert_eq!(control.memory().used(), 0);
    let bytes = encode(retained.as_ref(), 1, &control).unwrap();
    assert!(matches!(
        decode_document_image(&bytes, &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_document_images_reject_incomplete_invalid_and_underfunded_input() {
    let mut source = MemoryDocumentStore::new();
    source
        .put_stored(
            1,
            StoredDocument::with_metadata(
                Document::from([("name".into(), Value::Str("payload".into()))]),
                DocumentMetadata::with_tuple_xmin(0),
            ),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    let bytes = encode(&source, 1, &control).unwrap();
    for end in 0..bytes.len() {
        assert!(decode_document_image(&bytes[..end], &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    for offset in [7, 8, 20, 21] {
        let mut bad = bytes.clone();
        bad[offset] = 255;
        assert!(decode_document_image(&bad, &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(decode_document_image(&trailing, &control).is_err());
    let denied = StorageReadControl::with_limit(1);
    assert!(matches!(
        decode_document_image(&bytes, &denied),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        decode_document_image(&bytes, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    let mut output = Vec::new();
    assert!(matches!(
        source.write_image_document(1, &mut output, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(output.is_empty());
    assert_eq!(control.memory().used(), 0);
}
