//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    backup::{
        BackupCatalogField, BackupCatalogTextInput, BackupError, BackupImageSource, BackupResult,
    },
    catalog::input::{CatalogInputLimits, CatalogInputStep},
    read_control::StorageReadControl,
    PersistentStorageProvider,
};

const PREFIX: &str = "cut%_\0기:";

fn open(
    image: &dyn BackupImageSource,
    maximum: usize,
    control: &StorageReadControl,
) -> BackupResult<BackupCatalogTextInput> {
    image
        .stored_catalog_text(
            BackupCatalogField::MetadataPrefix(PREFIX),
            CatalogInputLimits {
                maximum_key_bytes: 1024,
                maximum_value_bytes: maximum,
            },
            control,
        )?
        .ok_or(BackupError::Invalid("missing metadata value input"))
}

fn collect(input: &mut BackupCatalogTextInput) -> BackupResult<Vec<Vec<u8>>> {
    let mut output = Vec::new();
    loop {
        let before = input.statistics();
        let step = input.advance()?;
        let after = input.statistics();
        if after.visited_positions - before.visited_positions > 1
            || after.metadata_read_bytes - before.metadata_read_bytes > 1024
        {
            return Err(BackupError::Invalid("unbounded metadata projection"));
        }
        match step {
            CatalogInputStep::Pending => (),
            CatalogInputStep::Row(value) => output.push(value.to_vec()),
            CatalogInputStep::Complete => {
                output.sort();
                return Ok(output);
            }
        }
    }
}

/// Exercise raw metadata on a retained provider image through replacements, deletions, new records and an unrelated value that exceeds the entire query allowance.
pub fn verify_backup_metadata_values(provider: &dyn PersistentStorageProvider) -> BackupResult<()> {
    let capture = StorageReadControl::with_limit(2 << 20);
    let control = StorageReadControl::with_limit(64 << 10);
    let db = provider.open_session()?;
    let initial = provider.capture_backup_image(&capture)?;
    let original = format!("{}한🌱\0é", "x".repeat(1023));
    let first = format!("{PREFIX}변경");
    let retired = format!("{PREFIX}소멸");
    let new = format!("{PREFIX}추가");
    let old_tombstone = format!("{PREFIX}폐기");
    db.catalog.set_metadata(PREFIX, "")?;
    db.catalog.set_metadata(&first, &original)?;
    db.catalog.set_metadata(&retired, "removed")?;
    db.catalog.set_metadata(&old_tombstone, "wrong")?;
    db.catalog.delete_metadata(&old_tombstone)?;
    let exact = "declarations%_\0기";
    db.catalog.set_metadata(exact, "retained definition")?;
    db.catalog
        .set_metadata(&format!("{exact}.opaque"), &"x".repeat(96 << 10))?;
    db.catalog
        .set_metadata("cut%_\0거:unrelated", &"z".repeat(256 << 10))?;
    let pinned = provider.capture_backup_image(&capture)?;
    db.catalog.delete_metadata(exact)?;
    db.catalog.delete_metadata(&retired)?;
    db.catalog.set_metadata(&first, "replacement")?;
    db.catalog.set_metadata(&new, "new")?;
    let current = provider.capture_backup_image(&capture)?;
    for (image, expected) in [
        (&pinned, vec![b"retained definition".to_vec()]),
        (&current, Vec::new()),
    ] {
        let limits = CatalogInputLimits {
            maximum_key_bytes: 1024,
            maximum_value_bytes: 32,
        };
        let mut input = image
            .stored_catalog_text(BackupCatalogField::Metadata(exact), limits, &control)?
            .ok_or(BackupError::Invalid("missing exact metadata input"))?;
        if collect(&mut input)? != expected {
            return Err(BackupError::Invalid(
                "exact catalog metadata changed across the cut",
            ));
        }
    }
    let mut old = open(&pinned, 2048, &control)?;
    let mut expected = vec![Vec::new(), original.into_bytes(), b"removed".to_vec()];
    expected.sort();
    if !collect(&mut open(&initial, 2048, &control)?)?.is_empty()
        || collect(&mut old)? != expected
        || old.statistics().metadata_read_bytes >= 2048
        || collect(&mut open(&current, 2048, &control)?)?
            != [b"".as_slice(), b"new", b"replacement"]
    {
        return Err(BackupError::Invalid(
            "captured metadata value membership or bytes",
        ));
    }
    if !matches!(
        collect(&mut open(&pinned, 32, &control)?),
        Err(BackupError::Memory(_))
    ) || control.memory().used() != 0
    {
        return Err(BackupError::Invalid(
            "metadata value limit or cursor retirement",
        ));
    }
    let mut input = open(&pinned, 2048, &control)?;
    control.cancellation().cancel();
    if !matches!(input.advance(), Err(BackupError::Cancelled(_)))
        || !matches!(input.advance(), Err(BackupError::Finished))
        || control.memory().used() != 0
    {
        return Err(BackupError::Invalid("metadata projection cancellation"));
    }
    let independent = StorageReadControl::with_limit(64 << 10);
    let mut input = open(&pinned, 2048, &independent)?;
    capture.cancellation().cancel();
    if !matches!(input.advance(), Err(BackupError::Cancelled(_)))
        || independent.memory().used() != 0
    {
        return Err(BackupError::Invalid(
            "metadata projection lost source cancellation",
        ));
    }
    Ok(())
}
