//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_document_image_checks_original_cancellation_between_writes() {
    struct Output {
        original: uqa_core::CancellationToken,
        bytes: usize,
    }
    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes += bytes.len();
            if self.bytes >= 8192 {
                self.original.cancel();
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let original = StorageReadControl::with_limit(1024);
    let invoking = StorageReadControl::with_limit(1024);
    let mut source = MemoryDocumentStore::new();
    source
        .put(
            1,
            std::collections::BTreeMap::from([("payload".into(), Value::Bytes(vec![3; 1 << 20]))]),
        )
        .unwrap();
    let retained = ReadOnlySnapshot(
        source.snapshot().unwrap(),
        None,
        Some(original.clone()),
        None,
    );
    let mut output = Output {
        original: original.cancellation().clone(),
        bytes: 0,
    };
    assert!(matches!(
        retained.write_image_document(1, &mut output, &invoking),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    assert!((8192..32768).contains(&output.bytes));
    assert!(!invoking.cancellation().is_cancelled());
    assert_eq!(original.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
}
