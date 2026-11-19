//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact metadata reads retain native text without collecting neighboring catalog records.

use super::{expect, expect_eq};
use crate::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, PersistentStorageSession,
    StorageBackendError, StorageBackendResult, StorageSavepointId,
};

/// Exercise exact text selection, current private state, allocation retirement and resource failures on a disposable provider session.
pub fn verify_controlled_metadata_reads(
    source: PersistentStorageSession,
) -> StorageBackendResult<()> {
    let name = "retained:%_\0한글";
    let original = " { \"n\": 1.00, \"s\": \"exact\\u0000\" } ";
    let private = " [ -0.0, \"private\" ] ";
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 4096,
    };
    let control = StorageReadControl::with_limit(65536);
    source.catalog.initialize_storage()?;
    source.catalog.set_metadata(name, original)?;
    source.catalog.set_metadata("retained:empty", "")?;
    source
        .catalog
        .set_metadata("unrelated:large", &"x".repeat(8192))?;
    source
        .catalog
        .set_metadata(&format!("{name}:suffix"), &"y".repeat(8192))?;
    let value = source
        .catalog
        .get_metadata_with_control(name, limits, &control)?
        .ok_or_else(|| StorageBackendError::Other("selected metadata is missing".into()))?;
    expect_eq(
        &value.as_str(),
        &original,
        "metadata text preserves its original bytes",
    )?;
    expect(
        value.budget().shares_allowance(control.memory()),
        "metadata retains the original allowance",
    )?;
    expect_eq(
        &control.memory().used(),
        &value.capacity(),
        "only the returned text outlives its read",
    )?;
    drop(value);
    verify_empty_and_missing(&source, limits, &control)?;
    source.backend.begin_transaction()?;
    let savepoint = StorageSavepointId::allocate();
    source.backend.savepoint(savepoint)?;
    source.catalog.set_metadata(name, private)?;
    let retained = source
        .catalog
        .get_metadata_with_control(name, limits, &control)?
        .ok_or_else(|| StorageBackendError::Other("private metadata is missing".into()))?;
    expect_eq(
        &retained.as_str(),
        &private,
        "metadata reads include the original private replacement",
    )?;
    source.catalog.delete_metadata(name)?;
    expect(
        source
            .catalog
            .get_metadata_with_control(name, limits, &control)?
            .is_none(),
        "private deletion hides committed text",
    )?;
    source.backend.rollback_to_savepoint(savepoint)?;
    let restored = source
        .catalog
        .get_metadata_with_control(name, limits, &control)?;
    expect_eq(
        &restored.as_deref().map(String::as_str),
        &Some(original),
        "savepoint rollback restores the original record",
    )?;
    drop(restored);
    source.backend.rollback_transaction()?;

    verify_denied_reads(&source, name, limits)?;
    drop(source);
    control.cancellation().cancel();
    expect_eq(
        &retained.as_str(),
        &private,
        "retained text survives source retirement and later cancellation",
    )?;
    expect_eq(
        &control.memory().used(),
        &retained.capacity(),
        "only the original private text remains admitted",
    )?;
    drop(retained);
    expect_eq(
        &control.memory().used(),
        &0,
        "last metadata owner releases its original allowance",
    )
}

fn verify_denied_reads(
    source: &PersistentStorageSession,
    name: &str,
    limits: CatalogInputLimits,
) -> StorageBackendResult<()> {
    for limited in [
        CatalogInputLimits {
            maximum_key_bytes: 1,
            ..limits
        },
        CatalogInputLimits {
            maximum_value_bytes: 1,
            ..limits
        },
    ] {
        let control = StorageReadControl::with_limit(65536);
        expect(
            matches!(
                source
                    .catalog
                    .get_metadata_with_control(name, limited, &control),
                Err(StorageBackendError::Memory(_))
            ),
            "physical record limits reject metadata before returning a value",
        )?;
        expect_eq(
            &control.memory().used(),
            &0,
            "rejected metadata retires every allocation",
        )?;
    }
    let denied = StorageReadControl::with_limit(0);
    expect(
        matches!(
            source
                .catalog
                .get_metadata_with_control(name, limits, &denied),
            Err(StorageBackendError::Memory(_))
        ),
        "metadata producer uses the supplied memory allowance",
    )?;
    expect_eq(
        &denied.memory().used(),
        &0,
        "denied metadata retains no allocation",
    )?;
    let cancelled = StorageReadControl::with_limit(65536);
    cancelled.cancellation().cancel();
    expect(
        matches!(
            source
                .catalog
                .get_metadata_with_control(name, limits, &cancelled),
            Err(StorageBackendError::Cancelled(_))
        ),
        "metadata reads preserve input cancellation",
    )?;
    expect_eq(
        &cancelled.memory().used(),
        &0,
        "cancelled metadata allocates nothing",
    )?;
    Ok(())
}

fn verify_empty_and_missing(
    source: &PersistentStorageSession,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    for (name, expected) in [("retained:empty", Some("")), ("missing", None)] {
        let value = source
            .catalog
            .get_metadata_with_control(name, limits, control)?;
        expect_eq(
            &value.as_deref().map(String::as_str),
            &expected,
            "empty and missing metadata remain distinct",
        )?;
        drop(value);
        expect_eq(
            &control.memory().used(),
            &0,
            "empty and absent reads retain no producer scratch",
        )?;
    }
    Ok(())
}
