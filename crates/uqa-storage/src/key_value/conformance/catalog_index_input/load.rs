//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete index loading preserves private views, ordinary order and terminal ownership.

use super::{
    equal, expect, expect_eq, prepare, row, CatalogInputLimits, PersistentStorageSession,
    StorageBackendResult, StorageReadControl,
};
use crate::catalog::index_load::{CatalogIndexLoad, CatalogIndexes};

fn finish(input: &mut CatalogIndexLoad) -> StorageBackendResult<CatalogIndexes> {
    for _ in 0..65536 {
        if let Some(rows) = input.advance(1)? {
            return Ok(rows);
        }
    }
    Err(crate::StorageBackendError::Other(
        "catalog index loading stopped progressing".into(),
    ))
}

/// Verify complete original provider order and payloads after private rollback, peer replacement and source retirement, then retire interrupted input before later writes.
pub fn verify_catalog_index_load(source: PersistentStorageSession) -> StorageBackendResult<()> {
    prepare(&source)?;
    let expected = source.catalog.load_catalog_indexes()?;
    let control = StorageReadControl::with_limit(65536);
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 8192,
    };
    let mut input = source.catalog.begin_index_load(limits, &control)?;
    expect(
        input.advance(1)?.is_none(),
        "index load retains a partial source",
    )?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    let mut changed = row("a");
    changed.definition_json = Some("later definition".into());
    later.catalog.save_catalog_index_row(&changed)?;
    let owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "index load does not retain its source catalog",
    )?;
    let actual = finish(&mut input)?;
    expect_eq(
        &actual.len(),
        &expected.len(),
        "complete selected index membership",
    )?;
    for (actual, expected) in actual.iter().zip(&expected) {
        equal(actual, expected)?;
    }
    let occupied = control.memory().reserve(control.memory().available())?;
    control.cancellation().cancel();
    later
        .catalog
        .save_catalog_index_row(&row("after_completed_input"))?;
    expect(
        input.advance(1).is_err(),
        "completed load cannot replay its physical source",
    )?;
    drop((actual, input));
    expect_eq(
        &control.memory().used(),
        &occupied.bytes(),
        "index load releases all original rows",
    )?;
    drop(occupied);
    expect_eq(
        &control.memory().used(),
        &0,
        "completed index load releases admission",
    )?;
    terminal(&later, limits)
}

fn terminal(
    source: &PersistentStorageSession,
    limits: CatalogInputLimits,
) -> StorageBackendResult<()> {
    for outcome in ["cancel", "memory", "zero", "drop", "unwind", "record_limit"] {
        let control = StorageReadControl::with_limit(65536);
        let limits = if outcome == "record_limit" {
            CatalogInputLimits {
                maximum_value_bytes: 1,
                ..limits
            }
        } else {
            limits
        };
        let mut input = source.catalog.begin_index_load(limits, &control)?;
        if outcome != "record_limit" {
            expect(
                input.advance(3)?.is_none(),
                "terminal load retains a source prefix",
            )?;
        }
        let occupied = (outcome == "memory")
            .then(|| control.memory().reserve(control.memory().available()))
            .transpose()?;
        if outcome == "cancel" {
            control.cancellation().cancel();
        }
        if outcome == "unwind" {
            expect(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("abandoned index catalog load");
                }))
                .is_err(),
                "unwinding consumes index input",
            )?;
        } else {
            if outcome != "drop" {
                expect(
                    fail(&mut input, usize::from(outcome != "zero"))?,
                    "terminal index input must fail",
                )?;
                expect(input.advance(1).is_err(), "failed index load cannot resume")?;
            }
            drop(input);
        }
        drop(occupied);
        expect_eq(
            &control.memory().used(),
            &0,
            "terminal load releases rows and source",
        )?;
        source.catalog.save_catalog_index_row(&row(outcome))?;
    }
    Ok(())
}

fn fail(input: &mut CatalogIndexLoad, steps: usize) -> StorageBackendResult<bool> {
    for _ in 0..65536 {
        match input.advance(steps) {
            Err(_) => return Ok(true),
            Ok(Some(_)) => return Ok(false),
            Ok(None) => (),
        }
    }
    Err(crate::StorageBackendError::Other(
        "terminal index input stopped progressing".into(),
    ))
}
