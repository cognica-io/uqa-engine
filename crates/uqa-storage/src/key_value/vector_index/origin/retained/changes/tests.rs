//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::format::DiskANNVectorVersion;
use crate::key_value::{KeyValueRead, KeyValueReadRevision};
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::read_control::{KeyReadVisitor, KeyValueReadVisitor, ValueReadVisitor};
use std::sync::Arc;

struct FaultyRead {
    mode: u8,
    control: StorageReadControl,
    record: Record,
}

impl KeyValueRead for FaultyRead {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        Ok(KeyValueReadRevision::fresh())
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("change reads must enforce encoded limits")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("change enumeration must not materialize a value prefix")
    }
    fn visit_value_bounded(
        &self,
        key: &[u8],
        _: usize,
        _: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if key.starts_with(b"origins") {
            return visit(Some(&self.record.encode()));
        }
        assert!(key.starts_with(b"changes"));
        match self.mode {
            1 => {
                visit(Some(&self.record.encode()))?;
                let _ignored = visit(None);
            }
            2 => {}
            4 => {
                let _ignored = visit(Some(&Record::new(self.record.version(), 2, 2)?.encode()));
            }
            _ => visit(Some(&self.record.encode()))?,
        }
        Ok(())
    }
    fn visit_keys_after(
        &self,
        prefix: &[u8],
        _: Option<&[u8]>,
        _: usize,
        _: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let mut key = prefix.to_vec();
        if prefix == b"changes" {
            key.extend_from_slice(&DiskANNChangeIdentity::new(7, self.record.version()).encode());
            if self.mode == 3 {
                let _ignored = visit(b"changes-invalid");
            }
            let _ignored = visit(&key);
            if self.mode == 0 {
                let _ignored = visit(&key);
            }
        } else {
            assert!(prefix.starts_with(b"vectors"));
            key.extend_from_slice(&0_u64.to_be_bytes());
            visit(&key)?;
        }
        Ok(())
    }
}

#[test]
fn diskann_change_cursor_preserves_suppressed_errors_and_requires_complete_point_reads() {
    let writer = StorageTransactionId::new(DatabaseId::from_bytes([7; 16]), 9).unwrap();
    let version = DiskANNVectorVersion::new(writer, 1).unwrap();
    for (mode, message) in [
        (0, "exceeded its requested page"),
        (1, "returned more than once"),
        (2, "was not returned"),
        (3, "invalid change identity width"),
        (4, "differs from its canonical origin"),
    ] {
        let control = StorageReadControl::with_limit(8192);
        let source = RetainedDiskANNCanonical::new(
            Arc::new(FaultyRead {
                mode,
                control: control.clone(),
                record: Record::new(version, 2, 1).unwrap(),
            }),
            b"vectors",
            b"origins",
            b"changes",
            2,
            &control,
        )
        .unwrap();
        let query = StorageReadControl::with_limit(8192);
        let error = source.next_change_after(None, &query).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
        assert_eq!(query.memory().used(), 0);
    }
}
