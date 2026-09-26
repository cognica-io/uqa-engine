//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Damage actual selected records without duplicating their physical addresses outside the owner.

use super::{
    expect_eq, DiskANNGeneration, DiskANNRecordKey, KeyValueStore, Keys, Kind, StorageBackendError,
    StorageBackendResult,
};
use crate::key_value::diskann::{keys::ROOT, publication::HEAD_PREFIX};

pub(crate) fn verify_selected_corruption(
    store: &dyn KeyValueStore,
    generation: DiskANNGeneration,
    check: &mut dyn FnMut() -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    let heads = store.scan_prefix(HEAD_PREFIX)?;
    expect_eq(&heads.len(), &1, "one selected fixture generation")?;
    let keys = Keys::new(generation);
    // The first retained traversal must load a graph page after its current record is gone.
    let mut cases = vec![
        ("graph", keys.key(Kind::Graph(0)).as_ref().to_vec()),
        ("head", heads[0].0.clone()),
    ];
    cases.extend(
        [
            ("state", Kind::State),
            ("manifest", Kind::Record(DiskANNRecordKey::Manifest)),
            ("codebook", Kind::Record(DiskANNRecordKey::Codebook)),
            ("codes", Kind::Record(DiskANNRecordKey::Codes(0))),
            ("side", Kind::Record(DiskANNRecordKey::Side(0))),
            ("origins", Kind::Record(DiskANNRecordKey::Origins(0))),
        ]
        .map(|(name, kind)| (name, keys.key(kind).as_ref().to_vec())),
    );
    for (name, key) in cases {
        let original = store
            .get(&key)?
            .filter(|bytes| !bytes.is_empty())
            .ok_or_else(|| StorageBackendError::Other(format!("missing fixture {name}")))?;
        let mut damaged = original.clone();
        damaged[0] ^= 0xff;
        for bytes in [None, Some(damaged.as_slice())] {
            match bytes {
                None => store.delete(&key)?,
                Some(bytes) => store.put(&key, bytes)?,
            }
            let before = store.scan_prefix(ROOT)?;
            check().map_err(|error| {
                StorageBackendError::Other(format!(
                    "selected {name} (missing: {}): {error}",
                    bytes.is_none()
                ))
            })?;
            expect_eq(
                &store.scan_prefix(ROOT)?,
                &before,
                "failed restore/search cannot repair or replace physical artifacts",
            )?;
        }
        store.put(&key, &original)?;
    }
    Ok(())
}
