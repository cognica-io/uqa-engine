//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed committed snapshots combine exact private record keys, not payload equality.

use std::collections::BTreeMap;

use super::{expect, expect_eq};
use crate::{
    CatalogFacade, PersistentStorageSession, StorageBackendError, StorageBackendResult,
    StorageSavepointId,
};

fn rows(
    catalog: &dyn CatalogFacade,
    prefix: &str,
) -> StorageBackendResult<BTreeMap<String, String>> {
    let mut rows = BTreeMap::new();
    catalog.visit_metadata_with_prefix(prefix, &mut |key, value| {
        rows.insert(key.to_owned(), value.to_owned());
        Ok(())
    })?;
    Ok(rows)
}

/// Exercise exact physical-key histories through the paired catalog/backend capability, including provider forwarding and transaction lifetime checks.
pub fn verify_catalog_read_boundaries(
    source: &PersistentStorageSession,
) -> StorageBackendResult<()> {
    let peer = source.backend.open_session()?;
    let cancellation = uqa_core::CancellationToken::new();
    for history in ["add", "rename", "drop", "equal_payload", "unrelated"] {
        verify_history(source, &peer, &cancellation, history)?;
    }
    verify_read_controls(source)
}

fn verify_history(
    source: &PersistentStorageSession,
    peer: &PersistentStorageSession,
    cancellation: &uqa_core::CancellationToken,
    history: &str,
) -> StorageBackendResult<()> {
    let prefix = format!("fixed-catalog:{history}:");
    let original = format!("{prefix}original-key");
    let external = format!("{prefix}external-key");
    let own = format!("{prefix}own-key");
    source.catalog.set_metadata(&original, "original")?;
    source.backend.begin_transaction()?;
    let boundary = source.backend.capture_committed_read_boundary()?;
    peer.backend.begin_transaction()?;
    if history != "unrelated" {
        peer.catalog.delete_metadata(&original)?;
    }
    peer.catalog.set_metadata(
        &external,
        if history == "equal_payload" {
            "original"
        } else {
            "external"
        },
    )?;
    peer.backend.commit_transaction()?;
    source.backend.refresh_transaction_snapshot(cancellation)?;
    let savepoint = StorageSavepointId::allocate();
    source.backend.savepoint(savepoint)?;
    match history {
        "rename" | "equal_payload" | "drop" => source.catalog.delete_metadata(&external)?,
        "unrelated" => source.catalog.delete_metadata(&original)?,
        _ => {}
    }
    if history != "drop" {
        source.catalog.set_metadata(&own, "own")?;
    }
    let mut expected = BTreeMap::new();
    if history != "unrelated" {
        expected.insert(original.clone(), "original".into());
    }
    if history != "drop" {
        expected.insert(own, "own".into());
    }
    let retained = source
        .backend
        .open_read_session_with_private_changes(&boundary, cancellation)?;
    expect_eq(
        &rows(&*retained.catalog, &prefix)?,
        &expected,
        "fixed base plus exact current private keys",
    )?;
    expect(
        retained.catalog.set_metadata("forbidden", "write").is_err(),
        "composed reader cannot publish",
    )?;
    expect(
        peer.backend
            .open_read_session_with_private_changes(&boundary, cancellation)
            .is_err(),
        "foreign session cannot attach private changes",
    )?;
    source.backend.rollback_to_savepoint(savepoint)?;
    let restored = source
        .backend
        .open_read_session_with_private_changes(&boundary, cancellation)?;
    expect_eq(
        &rows(&*restored.catalog, &prefix)?,
        &BTreeMap::from([(original.clone(), "original".into())]),
        "savepoint restores private root without advancing the fixed base",
    )?;
    expect_eq(
        &rows(&*retained.catalog, &prefix)?,
        &expected,
        "existing reader retains its discarded private branch",
    )?;
    source.backend.commit_transaction()?;
    let current = rows(&*source.catalog, &prefix)?;
    expect(
        current.contains_key(&external),
        "ordinary current catalog sees the peer commit",
    )?;
    expect_eq(
        &current.contains_key(&original),
        &(history == "unrelated"),
        "ordinary current catalog retains its actual original key",
    )?;
    verify_completed_source(source, &boundary, cancellation)?;
    let nested = retained.backend.open_retained_read_session(cancellation)?;
    drop(retained);
    expect_eq(
        &rows(&*nested.catalog, &prefix)?,
        &expected,
        "nested reader survives source completion and intermediate owner drop",
    )
}

fn verify_completed_source(
    source: &PersistentStorageSession,
    boundary: &crate::mvcc::CommittedReadBoundary,
    cancellation: &uqa_core::CancellationToken,
) -> StorageBackendResult<()> {
    expect(
        source
            .backend
            .open_read_session_with_private_changes(boundary, cancellation)
            .is_err(),
        "completed transaction cannot attach new private changes",
    )?;
    source.backend.begin_transaction()?;
    expect(
        source
            .backend
            .open_read_session_with_private_changes(boundary, cancellation)
            .is_err(),
        "new transaction at the same committed sequence cannot reuse an earlier boundary",
    )?;
    source.backend.rollback_transaction()
}

fn verify_read_controls(source: &PersistentStorageSession) -> StorageBackendResult<()> {
    let cancellation = uqa_core::CancellationToken::new();
    source.backend.begin_transaction()?;
    let control = source
        .backend
        .retention_control()
        .ok_or_else(|| StorageBackendError::Other("missing retention control".into()))?;
    let reservation = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())?;
    expect(
        matches!(
            source.backend.capture_committed_read_boundary(),
            Err(StorageBackendError::Memory(_))
        ),
        "boundary retention uses the original exhausted allowance",
    )?;
    drop(reservation);
    let boundary = source.backend.capture_committed_read_boundary()?;
    cancellation.cancel();
    expect(
        matches!(
            source
                .backend
                .open_read_session_with_private_changes(&boundary, &cancellation),
            Err(StorageBackendError::Cancelled(_))
        ),
        "cancelled capture keeps its typed error",
    )?;
    cancellation.reset();
    source
        .backend
        .open_read_session_with_private_changes(&boundary, &cancellation)?;
    source.backend.rollback_transaction()?;
    Ok(())
}
