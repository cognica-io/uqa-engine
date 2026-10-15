//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata visitors retain the store's committed/private boundary and error identity.

use super::{expect, expect_eq};
use crate::{CatalogFacade, KeyValueCatalog, KeyValueStore, StorageBackendResult};
use std::{collections::BTreeMap, sync::Arc};

/// Verify borrowed catalog scans against a fresh versioned store and an independent peer.
pub fn verify_catalog_metadata_visits(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    let peer = KeyValueCatalog::new(store.open_session()?);
    let prefix = "visit:%_\0日本語:";
    let a = format!("{prefix}a");
    let b = format!("{prefix}b");
    catalog.set_metadata(&a, "original-a")?;
    catalog.set_metadata(&b, "original-b")?;
    catalog.set_metadata("visit:unrelated", "outside")?;
    let read = || {
        let mut rows = BTreeMap::new();
        catalog.visit_metadata_with_prefix(prefix, &mut |key, value| {
            rows.insert(key.to_owned(), value.to_owned());
            Ok(())
        })?;
        let mut fixed = BTreeMap::new();
        catalog.visit_metadata_with_key_length(prefix, prefix.len() + 1, &mut |key, value| {
            fixed.insert(key.to_owned(), value.to_owned());
            Ok(())
        })?;
        expect_eq(
            &fixed,
            &rows,
            "fixed-width seek preserves the same metadata boundary",
        )?;
        Ok::<_, crate::StorageBackendError>(rows)
    };
    let original = read()?;
    expect_eq(&original.len(), &2, "literal metadata prefix")?;
    store.begin_transaction()?;
    expect_eq(&read()?, &original, "initial pinned metadata view")?;
    store.savepoint("metadata-visits")?;
    catalog.set_metadata(&a, "private-a")?;
    peer.set_metadata(&b, "peer-b")?;
    expect_eq(
        &read()?,
        &BTreeMap::from([
            (a.clone(), "private-a".into()),
            (b.clone(), "original-b".into()),
        ]),
        "private replacement preserves the pinned committed view",
    )?;
    catalog.delete_metadata(&a)?;
    expect_eq(
        &read()?,
        &BTreeMap::from([(b.clone(), "original-b".into())]),
        "private deletion masks only its exact metadata key",
    )?;
    store.rollback_to_savepoint("metadata-visits")?;
    expect_eq(&read()?, &original, "savepoint restores the pinned rows")?;
    store.commit_transaction()?;
    expect_eq(
        &read()?,
        &BTreeMap::from([(a, "original-a".into()), (b, "peer-b".into())]),
        "transaction end exposes the peer commit",
    )?;
    let mut count = 0;
    let error = catalog.visit_metadata_with_prefix(prefix, &mut |_, _| {
        count += 1;
        Err(uqa_core::QueryCancelled::USER_REQUEST.into())
    });
    expect(
        matches!(error, Err(crate::StorageBackendError::Cancelled(_))) && count == 1,
        "visitor cancellation keeps its identity and stops before the next row",
    )?;
    expect_eq(
        &read()?.len(),
        &2,
        "failed visitor leaves source records intact",
    )?;
    Ok(())
}
