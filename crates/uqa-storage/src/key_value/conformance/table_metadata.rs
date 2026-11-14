//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table metadata consumers preserve the selected provider state and exact lookup limits.

use super::{expect, expect_eq};
use crate::{
    catalog::input::CatalogInputLimits,
    document_store::identifiers::{
        legacy_document_id_metadata_key, load_legacy_document_id_watermark,
        load_legacy_document_id_watermark_from,
    },
    read_control::StorageReadControl,
    statistics_maintenance::StatisticsMaintenance,
    CatalogFacade, PersistentStorageSession, StorageBackendError, StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::{image_io::ImageDecoder, CancellationToken};

const TABLE: &str = "public.metadata_%한글";
const OBJECT: [u8; 16] = [7; 16];
const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 16384,
};

#[derive(Debug, PartialEq, Eq)]
enum Record {
    Maintenance(StatisticsMaintenance),
    Reservation(Option<u128>),
}

fn input(
    catalog: Arc<dyn CatalogFacade>,
    maintenance: bool,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    invoking: &CancellationToken,
) -> StorageBackendResult<ImageDecoder<Record, StorageBackendError>> {
    let memory = control.memory().clone();
    let control = control.clone();
    Ok(ImageDecoder::new(
        &memory,
        invoking,
        move |mut reader| async move {
            if maintenance {
                StatisticsMaintenance::load_for_from(
                    catalog.as_ref(),
                    TABLE,
                    OBJECT,
                    limits,
                    &control,
                    &mut reader.input(),
                )
                .await
                .map(Record::Maintenance)
            } else {
                load_legacy_document_id_watermark_from(
                    catalog.as_ref(),
                    TABLE,
                    limits,
                    &control,
                    &mut reader.input(),
                )
                .await
                .map(Record::Reservation)
            }
        },
    )?)
}

fn read(source: &PersistentStorageSession, maintenance: bool) -> StorageBackendResult<Record> {
    let control = StorageReadControl::with_limit(65536);
    let result = input(
        source.catalog.clone(),
        maintenance,
        LIMITS,
        &control,
        &CancellationToken::new(),
    )?
    .read(&mut std::io::empty());
    expect_eq(
        &control.memory().used(),
        &0,
        "scalar output retires all input memory",
    )?;
    result
}

/// Check missing, committed and private metadata, object identity, full-width reservations, rollback, original cancellation and key/value admission on a disposable persistent session.
pub fn verify_table_metadata_input(source: PersistentStorageSession) -> StorageBackendResult<()> {
    source.catalog.initialize_storage()?;
    let statistics_key = StatisticsMaintenance::key(TABLE);
    let reservation_key = legacy_document_id_metadata_key(TABLE);
    expect_eq(
        &read(&source, true)?,
        &Record::Maintenance(StatisticsMaintenance::default()),
        "missing maintenance defaults",
    )?;
    expect_eq(
        &read(&source, false)?,
        &Record::Reservation(None),
        "missing reservation stays absent",
    )?;
    source
        .catalog
        .set_metadata(&statistics_key, r#"{"changes":3,"analyzed_rows":8}"#)?;
    source.catalog.set_metadata(&reservation_key, "00017")?;
    let committed = read(&source, true)?;
    expect_eq(
        &committed,
        &Record::Maintenance(StatisticsMaintenance::load_for(
            source.catalog.as_ref(),
            TABLE,
            OBJECT,
        )?),
        "ordinary and controlled maintenance agree",
    )?;
    expect_eq(
        &read(&source, false)?,
        &Record::Reservation(load_legacy_document_id_watermark(
            source.catalog.as_ref(),
            TABLE,
        )?),
        "ordinary and controlled reservation agree",
    )?;
    source.backend.begin_transaction()?;
    source.catalog.set_metadata(
        &statistics_key,
        r#"{"changes":31,"analyzed_rows":43,"generation":11}"#,
    )?;
    source.catalog.set_metadata(
        &reservation_key,
        &format!("+{}18446744073709551616", "0".repeat(4096)),
    )?;
    let private = read(&source, true)?;
    expect(
        private != committed,
        "selected maintenance includes private replacement",
    )?;
    expect_eq(
        &read(&source, false)?,
        &Record::Reservation(Some(u128::from(u64::MAX) + 1)),
        "private reservation preserves exhaustion sentinel",
    )?;
    source.catalog.delete_metadata(&statistics_key)?;
    source.catalog.delete_metadata(&reservation_key)?;
    expect_eq(
        &read(&source, true)?,
        &Record::Maintenance(StatisticsMaintenance::default()),
        "private deletion hides maintenance",
    )?;
    expect_eq(
        &read(&source, false)?,
        &Record::Reservation(None),
        "private deletion hides reservation",
    )?;
    source.backend.rollback_transaction()?;
    expect_eq(
        &read(&source, true)?,
        &committed,
        "rollback restores maintenance",
    )?;
    expect_eq(
        &read(&source, false)?,
        &Record::Reservation(Some(17)),
        "rollback restores reservation",
    )?;
    verify_identity_and_format(&source)?;
    verify_failures(&source)?;
    drop(source);
    let Record::Maintenance(private) = private else {
        unreachable!()
    };
    expect(
        private.invalidates_existing_statistics(),
        "completed private state survives provider retirement",
    )
}

fn verify_identity_and_format(source: &PersistentStorageSession) -> StorageBackendResult<()> {
    let statistics_key = StatisticsMaintenance::key(TABLE);
    let reservation_key = legacy_document_id_metadata_key(TABLE);
    let other = format!(
        "{{\"object_id\":{},\"changes\":9,\"analyzed_rows\":1}}",
        serde_json::to_string(&[8u8; 16])?
    );
    source.catalog.set_metadata(&statistics_key, &other)?;
    expect_eq(
        &read(source, true)?,
        &Record::Maintenance(StatisticsMaintenance::default()),
        "another relation identity defaults",
    )?;
    source.catalog.set_metadata(
        &statistics_key,
        &other.replace("\"changes\":9", "\"changes\":null"),
    )?;
    expect(
        matches!(read(source, true), Err(StorageBackendError::Serde(_))),
        "malformed state fails before identity selection",
    )?;
    source.catalog.set_metadata(&statistics_key, "{}")?;
    source.catalog.set_metadata(&reservation_key, "")?;
    expect_eq(
        &read(source, false)?,
        &Record::Reservation(None),
        "empty reservation stays absent",
    )?;
    source.catalog.set_metadata(&reservation_key, "invalid")?;
    expect_eq(
        &read(source, false).unwrap_err().to_string(),
        &load_legacy_document_id_watermark(source.catalog.as_ref(), TABLE)
            .unwrap_err()
            .to_string(),
        "reservation failure keeps ordinary diagnostic",
    )?;
    source.catalog.set_metadata(&reservation_key, "1")?;
    Ok(())
}

fn verify_failures(source: &PersistentStorageSession) -> StorageBackendResult<()> {
    for maintenance in [false, true] {
        for limits in [
            CatalogInputLimits {
                maximum_key_bytes: 0,
                ..LIMITS
            },
            CatalogInputLimits {
                maximum_key_bytes: 1,
                ..LIMITS
            },
            CatalogInputLimits {
                maximum_value_bytes: 0,
                ..LIMITS
            },
        ] {
            let control = StorageReadControl::with_limit(65536);
            let result = input(
                source.catalog.clone(),
                maintenance,
                limits,
                &control,
                &CancellationToken::new(),
            )?
            .read(&mut std::io::empty());
            expect(result.is_err(), "invalid metadata limits fail")?;
            expect_eq(
                &control.memory().used(),
                &0,
                "invalid limits retire metadata input",
            )?;
        }
        verify_cancellation(source, maintenance)?;
        let control = StorageReadControl::with_limit(0);
        expect(
            input(
                source.catalog.clone(),
                maintenance,
                LIMITS,
                &control,
                &CancellationToken::new(),
            )
            .is_err(),
            "metadata continuation admits before allocation",
        )?;
        expect_eq(
            &control.memory().used(),
            &0,
            "denied construction retains nothing",
        )?;
        let control = StorageReadControl::with_limit(65536);
        let input = input(
            source.catalog.clone(),
            maintenance,
            LIMITS,
            &control,
            &CancellationToken::new(),
        )?;
        let blocker = control.memory().reserve(65536 - control.memory().used())?;
        expect(
            matches!(
                input.read(&mut std::io::empty()),
                Err(StorageBackendError::Memory(_))
            ),
            "exact lookup admits its key and payload",
        )?;
        drop(blocker);
        expect_eq(
            &control.memory().used(),
            &0,
            "denied lookup retires allocations",
        )?;
    }
    source.catalog.set_metadata(
        &StatisticsMaintenance::key(TABLE),
        &format!("{{\"ignored\":\"{}\"}}", "x".repeat(16384)),
    )?;
    source
        .catalog
        .set_metadata(&legacy_document_id_metadata_key(TABLE), &"0".repeat(16385))?;
    for maintenance in [false, true] {
        expect(
            matches!(
                read(source, maintenance),
                Err(StorageBackendError::Memory(_))
            ),
            "oversized selected record fails before materialization",
        )?;
    }
    Ok(())
}

fn verify_cancellation(
    source: &PersistentStorageSession,
    maintenance: bool,
) -> StorageBackendResult<()> {
    for original in [false, true] {
        let control = StorageReadControl::with_limit(65536);
        let invoking = CancellationToken::new();
        let mut input = input(
            source.catalog.clone(),
            maintenance,
            LIMITS,
            &control,
            &invoking,
        )?;
        if original {
            control.cancellation().cancel();
        } else {
            invoking.cancel();
        }
        let window = uqa_core::image_io::ImageDecodeWindow { bytes: 1, steps: 1 };
        expect(
            matches!(
                input.advance(&mut std::io::empty(), window),
                Err(StorageBackendError::Cancelled(_))
            ),
            "each cancellation owner terminates lookup",
        )?;
        expect(
            input.advance(&mut std::io::empty(), window).is_err(),
            "cancelled lookup cannot resume",
        )?;
        expect_eq(
            &control.memory().used(),
            &0,
            "cancelled lookup retires allocations",
        )?;
    }
    Ok(())
}
