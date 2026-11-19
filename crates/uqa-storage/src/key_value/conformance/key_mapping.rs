//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer-key certification keeps metadata presence and transaction visibility distinct.

use super::{expect, expect_eq};
use crate::{
    catalog::input::CatalogInputLimits,
    document_store::identifiers::key_mapping::{self, indexed_key, VERIFIED_KEY},
    read_control::StorageReadControl,
    CatalogFacade, PersistentStorageSession, StorageBackendError, StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    CancellationToken,
};

const OBJECT: [u8; 16] = [
    0, 1, 2, 3, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0xff, 0xfe, 0xfd, 0xfc,
];
const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };
const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 256,
    maximum_value_bytes: 256,
};

fn input(
    catalog: Arc<dyn CatalogFacade>,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    invoking: &CancellationToken,
) -> StorageBackendResult<ImageDecoder<bool, StorageBackendError>> {
    let memory = control.memory().clone();
    let control = control.clone();
    Ok(ImageDecoder::new(
        &memory,
        invoking,
        move |mut reader| async move {
            key_mapping::load_from(
                catalog.as_ref(),
                OBJECT,
                limits,
                &control,
                &mut reader.input(),
            )
            .await
        },
    )?)
}

fn read(source: &PersistentStorageSession) -> StorageBackendResult<bool> {
    let control = StorageReadControl::with_limit(65536);
    let result = input(
        source.catalog.clone(),
        LIMITS,
        &control,
        &CancellationToken::new(),
    )?
    .read(&mut std::io::empty());
    expect_eq(
        &control.memory().used(),
        &0,
        "mapping output retains no input allocation",
    )?;
    result
}

/// Verify ordinary and bounded mapping choices, exact object keys, absent/empty/nonempty markers, private changes, rollback and every pending cancellation/allocation boundary on a disposable session.
pub fn verify_integer_key_mapping_input(
    source: &PersistentStorageSession,
) -> StorageBackendResult<()> {
    source.catalog.initialize_storage()?;
    expect_eq(
        &indexed_key(OBJECT),
        &"uqa.integer_key_identities.indexed.v1:00010203123456789abcdef0fffefdfc".to_owned(),
        "object identity uses original lowercase hexadecimal bytes",
    )?;
    verify_presence(source)?;
    verify_visibility(source)?;
    verify_interruptions(source)?;
    verify_limits(source)
}

fn verify_presence(source: &PersistentStorageSession) -> StorageBackendResult<()> {
    for verified in [None, Some(""), Some("true"), Some("false")] {
        if let Some(value) = verified {
            source.catalog.set_metadata(VERIFIED_KEY, value)?;
        } else {
            source.catalog.delete_metadata(VERIFIED_KEY)?;
        }
        for indexed in [None, Some(""), Some("true"), Some("false")] {
            if let Some(value) = indexed {
                source.catalog.set_metadata(&indexed_key(OBJECT), value)?;
            } else {
                source.catalog.delete_metadata(&indexed_key(OBJECT))?;
            }
            let expected = verified.is_some() && indexed.is_none_or(str::is_empty);
            expect_eq(
                &read(source)?,
                &expected,
                "mapping keeps marker presence and emptiness",
            )?;
            expect_eq(
                &key_mapping::load(source.catalog.as_ref(), OBJECT)?,
                &expected,
                "ordinary mapping uses the same stored contract",
            )?;
        }
    }
    source.catalog.set_metadata(VERIFIED_KEY, "")?;
    source.catalog.delete_metadata(&indexed_key(OBJECT))?;
    source
        .catalog
        .set_metadata(&indexed_key([3; 16]), &"x".repeat(512))?;
    expect(read(source)?, "unrelated object marker is never read")
}

fn verify_visibility(source: &PersistentStorageSession) -> StorageBackendResult<()> {
    source.catalog.set_metadata(VERIFIED_KEY, "")?;
    source
        .catalog
        .set_metadata(&indexed_key(OBJECT), "indexed")?;
    source.backend.begin_transaction()?;
    source.catalog.delete_metadata(&indexed_key(OBJECT))?;
    let private = read(source)?;
    expect(private, "private marker deletion enables identity mapping")?;
    source.catalog.delete_metadata(VERIFIED_KEY)?;
    expect(
        !read(source)?,
        "private certification deletion disables identity mapping",
    )?;
    source.backend.rollback_transaction()?;
    expect(!read(source)?, "rollback restores original indexed mapping")?;
    expect(
        private,
        "completed private mapping is independent of later reads",
    )?;
    source.catalog.delete_metadata(&indexed_key(OBJECT))
}

fn verify_interruptions(source: &PersistentStorageSession) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(65536);
    let mut complete = input(
        source.catalog.clone(),
        LIMITS,
        &control,
        &CancellationToken::new(),
    )?;
    let mut pending = 0;
    loop {
        if let Some(mapped) = complete.advance(&mut std::io::empty(), WINDOW)? {
            expect(mapped, "complete mapping result")?;
            break;
        }
        pending += 1;
    }
    expect(pending >= 6, "both lookups consume bounded work")?;
    expect_eq(
        &control.memory().used(),
        &0,
        "completed mapping releases input",
    )?;
    for stop in 0..pending {
        for failure in 0..5 {
            let control = StorageReadControl::with_limit(65536);
            let invoking = CancellationToken::new();
            let mut input = input(source.catalog.clone(), LIMITS, &control, &invoking)?;
            for _ in 0..stop {
                expect(
                    input.advance(&mut std::io::empty(), WINDOW)?.is_none(),
                    "original pending mapping prefix",
                )?;
            }
            match failure {
                0 | 1 => {
                    if failure == 0 {
                        invoking.cancel();
                    } else {
                        control.cancellation().cancel();
                    }
                    expect(
                        matches!(
                            input.advance(&mut std::io::empty(), WINDOW),
                            Err(StorageBackendError::Cancelled(_))
                        ),
                        "each cancellation owner terminates mapping",
                    )?;
                    expect(
                        input.advance(&mut std::io::empty(), WINDOW).is_err(),
                        "failed mapping cannot resume",
                    )?;
                }
                2 => {
                    expect(
                        input
                            .advance(
                                &mut std::io::empty(),
                                ImageDecodeWindow { bytes: 0, steps: 1 },
                            )
                            .is_err(),
                        "invalid window retires mapping",
                    )?;
                }
                3 => {
                    let blocker = control.memory().reserve(65536 - control.memory().used())?;
                    match input.read(&mut std::io::empty()) {
                        Ok(mapped) => expect(mapped, "already admitted mapping may complete")?,
                        Err(StorageBackendError::Memory(_)) => (),
                        Err(error) => return Err(error),
                    }
                    drop(blocker);
                    expect_eq(
                        &control.memory().used(),
                        &0,
                        "denied mapping retires allocations",
                    )?;
                    continue;
                }
                4 => (),
                _ => unreachable!(),
            }
            drop(input);
            expect_eq(
                &control.memory().used(),
                &0,
                "interrupted mapping retires allocations",
            )?;
        }
    }
    Ok(())
}

fn verify_limits(source: &PersistentStorageSession) -> StorageBackendResult<()> {
    for limits in [
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
        expect(
            input(
                source.catalog.clone(),
                limits,
                &control,
                &CancellationToken::new(),
            )?
            .read(&mut std::io::empty())
            .is_err(),
            "invalid mapping record bound fails",
        )?;
        expect_eq(
            &control.memory().used(),
            &0,
            "invalid bound retires allocations",
        )?;
    }
    source.catalog.delete_metadata(VERIFIED_KEY)?;
    source
        .catalog
        .set_metadata(&indexed_key(OBJECT), &"x".repeat(512))?;
    expect(
        !read(source)?,
        "missing certification does not read oversized table marker",
    )?;
    source.catalog.set_metadata(VERIFIED_KEY, "")?;
    expect(
        matches!(read(source), Err(StorageBackendError::Memory(_))),
        "present certification reads the selected size-checked marker",
    )?;
    source
        .catalog
        .set_metadata(VERIFIED_KEY, &"x".repeat(512))?;
    expect(
        matches!(read(source), Err(StorageBackendError::Memory(_))),
        "certification text is bounded before materialization",
    )
}
