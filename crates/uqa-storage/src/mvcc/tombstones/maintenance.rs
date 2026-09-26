//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical layouts supply owned prefixes; common Storage schedules their finite retirement passes.

use crate::mvcc::{
    TombstoneReclamationRequest, TombstoneReclamationStep, VersionError, VersionResult,
    VersionedPersistence,
};
use crate::read_control::StorageReadControl;

pub fn reclaim_key_value_diskann_tombstones(
    store: &(impl VersionedPersistence + ?Sized),
    control: &StorageReadControl,
) -> VersionResult<()> {
    for prefix in crate::key_value::diskann_tombstone_prefixes() {
        reclaim_tombstone_prefix(store, prefix, control)?;
    }
    Ok(())
}

/// Run after ordinary history collection. Select a finite boundary including deleted metadata, then release this pass's own read lease before physical admission. A live reader or later revision is reconsidered on a future invocation.
pub fn reclaim_tombstone_prefix(
    store: &(impl VersionedPersistence + ?Sized),
    prefix: &[u8],
    control: &StorageReadControl,
) -> VersionResult<()> {
    control.check()?;
    let snapshot = store.snapshot(control)?;
    let through = snapshot.sequence();
    let request = TombstoneReclamationRequest {
        prefix,
        after: None,
        through,
    };
    request.validate(control)?;
    let mut present = false;
    snapshot.visit_keys(prefix, None, 1, control, &mut |_, _| {
        present = true;
        Ok(false)
    })?;
    drop(snapshot);
    if !present {
        return Ok(());
    }
    let mut after = None;
    loop {
        control.check()?;
        let request = TombstoneReclamationRequest {
            prefix,
            after: after.as_deref(),
            through,
        };
        match store.reclaim_tombstones(&request, control)? {
            TombstoneReclamationStep::Retained | TombstoneReclamationStep::Complete { .. } => {
                return Ok(())
            }
            TombstoneReclamationStep::More { after: cursor, .. } => {
                if !cursor.starts_with(prefix)
                    || after
                        .as_deref()
                        .is_some_and(|previous| &*cursor <= previous)
                {
                    return Err(VersionError::InvalidEncoding(
                        "unordered tombstone reclamation cursor",
                    ));
                }
                after = Some(cursor);
            }
        }
    }
}
