//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{expect, expect_eq};
use crate::{
    catalog::{
        input::CatalogInputLimits,
        schema::load::{CatalogSchemaLoad, CatalogSchemas},
    },
    read_control::StorageReadControl,
    PersistentStorageSession, SchemaRow, StorageBackendResult,
};
use uqa_core::catalog_schema::{BoundSchemaRow, SchemaTupleIdentity};

fn finish(input: &mut CatalogSchemaLoad) -> StorageBackendResult<CatalogSchemas> {
    for _ in 0..65536 {
        if let Some(rows) = input.advance(1)? {
            return Ok(rows);
        }
    }
    Err(crate::StorageBackendError::Other(
        "schema input stopped progressing".into(),
    ))
}

/// Verify a disposable provider's complete ordered schema input over its selected private view, independently of the originating session's later lifetime.
pub fn verify_catalog_schema_input(source: PersistentStorageSession) -> StorageBackendResult<()> {
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 65536,
    };
    source.catalog.initialize_storage()?;
    for (index, name) in ["z", "public", "a_longer", "한글\0schema", "removed"]
        .into_iter()
        .enumerate()
    {
        let mut row = BoundSchemaRow::bootstrap(name);
        row.tuple = Some(SchemaTupleIdentity::initial(
            40000 + u32::try_from(index).expect("small fixture"),
        ));
        source.catalog.save_schema_row(&row.into())?;
    }
    source.backend.begin_transaction()?;
    source
        .catalog
        .save_schema_row(&SchemaRow::legacy("public"))?;
    source
        .catalog
        .save_schema_row(&SchemaRow::bootstrap("private"))?;
    source.catalog.drop_schema("removed")?;
    let expected = source.catalog.load_schema_rows()?;
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = source.catalog.begin_schema_load(limits, &control)?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    later.catalog.drop_schema("z")?;
    later
        .catalog
        .save_schema_row(&SchemaRow::bootstrap("later"))?;
    let owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "schema input does not retain its source catalog handle",
    )?;
    let rows = finish(&mut input)?;
    expect(
        rows.iter().map(|row| &**row).eq(expected.iter()),
        "schema input preserves original provider order, values and private membership",
    )?;
    expect(
        input.advance(1).is_err(),
        "a completed schema load cannot reopen its source",
    )?;
    control.cancellation().cancel();
    expect(
        rows.iter().map(|row| &**row).eq(expected.iter()),
        "completed schemas survive original cancellation",
    )?;
    drop((rows, input));
    expect_eq(
        &control.memory().used(),
        &0,
        "schema retirement releases its complete input allowance",
    )?;
    verify_retirement(&later, limits)
}

fn verify_retirement(
    later: &PersistentStorageSession,
    limits: CatalogInputLimits,
) -> StorageBackendResult<()> {
    for terminal in ["cancel", "memory", "zero", "drop", "unwind", "encoded"] {
        let control = StorageReadControl::with_limit(1 << 20);
        let limits = if terminal == "encoded" {
            CatalogInputLimits {
                maximum_value_bytes: 1,
                ..limits
            }
        } else {
            limits
        };
        let mut input = later.catalog.begin_schema_load(limits, &control)?;
        if terminal != "encoded" {
            expect(
                input.advance(1)?.is_none(),
                "one native poll cannot materialize the complete schema catalog",
            )?;
        }
        let occupied = if terminal == "memory" {
            Some(control.memory().reserve(control.memory().available())?)
        } else {
            None
        };
        match terminal {
            "cancel" => {
                control.cancellation().cancel();
                expect(input.advance(1).is_err(), "cancelled schema input fails")?;
            }
            "zero" => expect(input.advance(0).is_err(), "empty schema window fails")?,
            "memory" | "encoded" => expect(
                finish(&mut input).is_err(),
                "schema decoding respects original memory and encoded value limits",
            )?,
            "unwind" => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("abandon schema input");
                }));
                drop(occupied);
                expect_eq(
                    &control.memory().used(),
                    &0,
                    "unwinding retires schema input",
                )?;
                continue;
            }
            "drop" => (),
            _ => unreachable!(),
        }
        if terminal != "drop" {
            expect(
                input.advance(1).is_err(),
                "failed schema input stays terminal",
            )?;
        }
        drop((input, occupied));
        expect_eq(
            &control.memory().used(),
            &0,
            "partial schema input releases all private rows and cursor owners",
        )?;
    }
    Ok(())
}
