//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::KeyValueReadRevision;
use crate::read_control::{KeyReadVisitor, KeyValueReadVisitor, ValueReadVisitor};

struct Read {
    control: StorageReadControl,
    keys: Vec<Vec<u8>>,
    reject: bool,
}

impl KeyValueRead for Read {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("discovery must not request a generation-sized revision")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("discovery must not read payloads")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("discovery must not read payloads")
    }
    fn visit_keys_after(
        &self,
        prefix: &[u8],
        _: Option<&[u8]>,
        limit: usize,
        _: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        assert_eq!(prefix, generation_prefix([1; 16]));
        assert_eq!(limit, 1);
        for key in &self.keys {
            let _ = visit(key); // Intentionally violate the provider's error propagation contract.
        }
        if self.reject {
            return Err(invalid("provider failed after visiting a key"));
        }
        Ok(())
    }
}

fn pass(keys: Vec<Vec<u8>>, reject: bool) -> KeyValueDiskANNMaintenance {
    let control = StorageReadControl::with_limit(8192);
    KeyValueDiskANNMaintenance {
        repository: None,
        read: Some(Arc::new(Read {
            control: control.clone(),
            keys,
            reject,
        })),
        database: Some([1; 16]),
        after: None,
        current: None,
        _memory: control
            .memory()
            .reserve(std::mem::size_of::<KeyValueDiskANNMaintenance>())
            .unwrap(),
        control,
    }
}

#[test]
fn diskann_discovery_rejects_swallowed_corrupt_duplicate_and_foreign_keys() {
    use super::super::keys::Kind;
    let generation = DiskANNGeneration::new([1; 16], 2, 3, 4).unwrap();
    let key = Keys::new(generation).key(Kind::State).as_ref().to_vec();
    let foreign = Keys::new(DiskANNGeneration::new([2; 16], 2, 3, 4).unwrap())
        .key(Kind::State)
        .as_ref()
        .to_vec();
    let orphan = Keys::new(generation).key(Kind::Graph(0)).as_ref().to_vec();
    for keys in [
        vec![key.clone(), key.clone()],
        vec![vec![], key.clone()],
        vec![key.clone(), vec![]],
        vec![foreign],
        vec![orphan],
    ] {
        let pass = pass(keys, false);
        assert!(pass.next_generation().is_err());
        assert!(pass.after.is_none());
        assert!(pass.current.is_none());
    }
    assert!(pass(vec![key.clone()], true).next_generation().is_err());
    let mut unordered = pass(vec![key.clone()], false);
    unordered.after = Some(generation);
    assert!(unordered.next_generation().is_err());
    assert_eq!(
        pass(vec![key], false).next_generation().unwrap(),
        Some(generation)
    );
    assert_eq!(pass(vec![], false).next_generation().unwrap(), None);
}
