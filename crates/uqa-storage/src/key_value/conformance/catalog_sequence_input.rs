//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{expect, expect_eq};
use crate::{
    catalog::{
        input::CatalogInputLimits,
        sequence_load::{CatalogSequenceLoad, CatalogSequences},
    },
    read_control::StorageReadControl,
    PersistentStorageSession, SequenceRow, StorageBackendResult,
};

fn finish(input: &mut CatalogSequenceLoad) -> StorageBackendResult<CatalogSequences> {
    for _ in 0..65536 {
        if let Some(rows) = input.advance(1)? {
            return Ok(rows);
        }
    }
    Err(crate::StorageBackendError::Other(
        "sequence input stopped progressing".into(),
    ))
}

fn sequence(name: &str, id: u8) -> SequenceRow {
    SequenceRow {
        relation: crate::RelationIdentity::new("public", name),
        security: crate::SequenceSecurityRow::bootstrap(),
        object_id: [id; 16],
        definition_generation: [id + 64; 16],
        start: 1,
        increment: 2,
        current: i64::from(id),
        called: false,
        log_count: 0,
        persistence: "p".into(),
        owner: None,
        options: crate::SequenceOptions {
            min_value: Some(1),
            max_value: Some(i64::MAX),
            cache_size: 3,
            ..crate::SequenceOptions::default()
        },
    }
}

/// Retain a complete private definition/value pair while the originating session rolls back and independent writers replace live values and membership.
pub fn verify_catalog_sequence_input(source: PersistentStorageSession) -> StorageBackendResult<()> {
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 65536,
    };
    source.catalog.initialize_storage()?;
    source.catalog.save_schema("public")?;
    for (index, name) in ["z", "a_longer", "한글_sequence", "removed"]
        .into_iter()
        .enumerate()
    {
        source
            .catalog
            .create_sequence_row(&sequence(name, u8::try_from(index).unwrap() + 1))?;
    }
    source.backend.begin_transaction()?;
    source
        .catalog
        .create_sequence_row(&sequence("private", 10))?;
    source.catalog.drop_sequence_row("public.removed")?;
    source
        .catalog
        .set_sequence_value("public.z", [1; 16], [65; 16], 701, true, 4)?;
    let expected = source.catalog.load_sequence_rows()?;
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = source.catalog.begin_sequence_load(limits, &control)?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    later
        .catalog
        .set_sequence_value("public.z", [1; 16], [65; 16], 999, true, 0)?;
    later.catalog.drop_sequence_row("public.a_longer")?;
    later.catalog.create_sequence_row(&sequence("later", 11))?;
    let owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "sequence input retains its selected read owner independently of the catalog handle",
    )?;
    let rows = finish(&mut input)?;
    expect(
        rows.iter().map(|row| &**row).eq(expected.iter()),
        "sequence input preserves ordered definitions and values from one selected view",
    )?;
    expect(
        input.advance(1).is_err(),
        "completed sequence input cannot reopen its source",
    )?;
    control.cancellation().cancel();
    expect(
        rows.iter().map(|row| &**row).eq(expected.iter()),
        "complete sequences survive original cancellation",
    )?;
    drop((rows, input));
    expect_eq(
        &control.memory().used(),
        &0,
        "sequence retirement releases the complete allowance",
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
        let mut input = later.catalog.begin_sequence_load(limits, &control)?;
        if terminal != "encoded" {
            expect(
                input.advance(1)?.is_none(),
                "one native poll cannot materialize the complete sequence catalog",
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
                expect(input.advance(1).is_err(), "cancelled sequence input fails")?;
            }
            "zero" => expect(input.advance(0).is_err(), "empty sequence window fails")?,
            "memory" | "encoded" => expect(
                finish(&mut input).is_err(),
                "sequence decoding respects original memory and encoded value limits",
            )?,
            "unwind" => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("abandon sequence input");
                }));
                drop(occupied);
                expect_eq(
                    &control.memory().used(),
                    &0,
                    "unwinding retires sequence input",
                )?;
                continue;
            }
            "drop" => (),
            _ => unreachable!(),
        }
        if terminal != "drop" {
            expect(
                input.advance(1).is_err(),
                "failed sequence input stays terminal",
            )?;
        }
        drop((input, occupied));
        expect_eq(
            &control.memory().used(),
            &0,
            "partial sequence input releases all private rows and cursor owners",
        )?;
    }
    Ok(())
}
