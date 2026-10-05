//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact selected catalog record identities across replacement and undo branches.

use super::{expect, expect_eq};
use crate::{
    catalog::CatalogRecordRef, CatalogFacade, KeyValueCatalog, KeyValueStore, StorageBackendResult,
};
use std::sync::Arc;

/// Verify record identity retention on a fresh disposable versioned store.
pub fn verify_catalog_record_revisions(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    let selected = [
        CatalogRecordRef::Metadata("revision-a"),
        CatalogRecordRef::Metadata("revision-missing"),
        CatalogRecordRef::Metadata("revision-a"),
    ];
    let read = || {
        catalog.record_revisions(&selected)?.ok_or_else(|| {
            crate::StorageBackendError::Other("versioned catalog has no record revisions".into())
        })
    };
    catalog.set_metadata("revision-a", "same")?;
    let original = read()?;
    expect(
        original[0].is_some() && original[1].is_none(),
        "existing and missing catalog identities",
    )?;
    expect_eq(
        &original[0],
        &original[2],
        "duplicate requests retain their order",
    )?;
    catalog.set_metadata("revision-b", "unrelated")?;
    expect_eq(&read()?, &original, "unrelated records preserve revisions")?;
    catalog.set_metadata("revision-a", "same")?;
    let replaced = read()?;
    expect(
        replaced != original,
        "identical committed replacement has a new identity",
    )?;
    store.begin_transaction()?;
    store.savepoint("revision-keep")?;
    catalog.set_metadata("revision-a", "same")?;
    let private = read()?;
    expect(
        private != replaced,
        "private replacement has a new identity",
    )?;
    store.rollback_to_savepoint("revision-keep")?;
    expect_eq(
        &read()?,
        &replaced,
        "savepoint restores the original identity",
    )?;
    catalog.set_metadata("revision-a", "same")?;
    expect(
        read()? != private,
        "new undo branch has a distinct identity",
    )?;
    store.rollback_transaction()?;
    expect_eq(
        &read()?,
        &replaced,
        "transaction rollback restores the committed identity",
    )?;
    catalog.delete_metadata("revision-a")?;
    expect(
        read()?.iter().all(Option::is_none),
        "deleted catalog records are missing",
    )?;
    Ok(())
}
