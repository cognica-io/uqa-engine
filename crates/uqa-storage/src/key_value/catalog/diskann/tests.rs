//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::read_control::{KeyValueReadVisitor, ValueReadVisitor};

struct Reader {
    control: StorageReadControl,
    mode: u8,
}

impl KeyValueRead for Reader {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        Ok(KeyValueReadRevision::fresh())
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("catalog binding requires a bounded read")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("catalog binding must not enumerate a prefix")
    }
    fn visit_value_bounded(
        &self,
        _: &[u8],
        _: usize,
        _: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        match self.mode {
            0 => visit(Some(b"42")),
            1 => Ok(()),
            2 => {
                visit(Some(b"42"))?;
                let _ = visit(Some(b"43"));
                Ok(())
            }
            _ => {
                let _ = visit(Some(b"invalid"));
                let _ = visit(Some(b"42"));
                Ok(())
            }
        }
    }
}

#[test]
fn diskann_catalog_binding_requires_record_provenance_and_exact_completion() {
    for mode in 0..4 {
        let reader = Reader {
            control: StorageReadControl::with_limit(8192),
            mode,
        };
        assert!(reader.record_revision(b"index").is_err());
        let result = decode::<u64>(&reader, b"index", &reader.control);
        if mode == 0 {
            let value = result.unwrap();
            assert_eq!(*value, 42);
            assert!(reader.control.memory().used() > 0);
            drop(value);
        } else {
            assert!(result.is_err());
        }
        assert_eq!(reader.control.memory().used(), 0);
    }
}

#[test]
fn diskann_catalog_binding_preserves_decode_quota_and_cancellation() {
    let reader = Reader {
        control: StorageReadControl::with_limit(8192),
        mode: 0,
    };
    let tiny = StorageReadControl::with_limit(1);
    assert!(matches!(
        decode::<u64>(&reader, b"index", &tiny),
        Err(crate::StorageBackendError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    reader.control.cancellation().cancel();
    assert!(matches!(
        decode::<u64>(&reader, b"index", &reader.control),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    assert_eq!(reader.control.memory().used(), 0);
}
