//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    backup::{BackupError, BackupImageSource, BackupResult, BackupSourceStatistics},
    catalog::input::{CatalogInputLimits, CatalogInputStep},
    read_control::StorageReadControl,
    PersistentStorageProvider, RelationSecurityRow, ViewRow,
};
use uqa_core::RelationIdentity;

fn collect(
    image: &dyn BackupImageSource,
    maximum_value_bytes: usize,
    control: &StorageReadControl,
) -> BackupResult<(Vec<Vec<u8>>, BackupSourceStatistics)> {
    let limits = CatalogInputLimits {
        maximum_key_bytes: 1024,
        maximum_value_bytes,
    };
    let mut input = image
        .stored_view_queries(limits, control)?
        .ok_or(BackupError::Invalid("missing persistent view input"))?;
    let mut output = Vec::new();
    loop {
        let previous = input.statistics();
        let step = input.advance()?;
        let statistics = input.statistics();
        if statistics.visited_positions - previous.visited_positions > 1
            || statistics.metadata_read_bytes - previous.metadata_read_bytes > 1024
            || statistics.copied_payload_bytes != 0
        {
            return Err(BackupError::Invalid("unbounded stored view projection"));
        }
        match step {
            CatalogInputStep::Pending => (),
            CatalogInputStep::Row(query) => output.push(query.to_vec()),
            CatalogInputStep::Complete => return Ok((output, statistics)),
        }
    }
}

/// Exercise the provider's actual joint cut, replacement, tombstones, post-capture creation and large evaluated rows under a small independent query allowance.
pub fn verify_backup_view_queries(provider: &dyn PersistentStorageProvider) -> BackupResult<()> {
    let source = provider.open_session()?;
    source.catalog.save_schema("public")?;
    let capture = StorageReadControl::with_limit(2 << 20);
    let control = StorageReadControl::with_limit(64 << 10);
    let empty = provider.capture_backup_image(&capture)?;
    let original = format!(
        r#"{{"ctes":[],"root":{{"text":"{}"}}}}"#,
        r#"🌱\"\""#.repeat(240)
    );
    let make = |name, definition_json| ViewRow {
        relation: RelationIdentity::new("public", name),
        security: RelationSecurityRow::bootstrap(),
        definition_json,
    };
    let rows = "z".repeat(256 << 10);
    let mut view = make(
        "first",
        format!(r#"{{"query":{original},"materialized_rows":[{{"text":"{rows}"}}]}}"#),
    );
    let removed = make("removed", original.clone());
    source.catalog.save_view(&removed)?;
    source.catalog.drop_view(&removed.relation)?;
    source.catalog.save_view(&view)?;
    let old = provider.capture_backup_image(&capture)?;
    view.definition_json = r#"{"query": { "replacement": true }}"#.into();
    source.catalog.save_view(&view)?;
    let legacy = r#"{ "ctes": [], "root": {"nested":["quoted \"key\""]} }"#;
    source
        .catalog
        .save_view(&make("later", legacy.to_owned()))?;

    let (absent, stats) = collect(&empty, 8192, &control)?;
    if !absent.is_empty() || stats.metadata_read_bytes != 0 {
        return Err(BackupError::Invalid(
            "views created after an empty backup cut",
        ));
    }
    let (retained, stats) = collect(&old, 8192, &control)?;
    if retained != [original.as_bytes()] || stats.metadata_read_bytes >= 16 << 10 {
        return Err(BackupError::Invalid(
            "stored query lost its cut or copied evaluated rows",
        ));
    }
    let current = provider.capture_backup_image(&capture)?;
    let (mut retained, _) = collect(&current, 8192, &control)?;
    retained.sort();
    let mut expected = [br#"{ "replacement": true }"#.as_slice(), legacy.as_bytes()];
    expected.sort();
    if retained != expected {
        return Err(BackupError::Invalid(
            "updated and legacy stored view queries",
        ));
    }

    if !matches!(collect(&old, 13, &control), Err(BackupError::Memory(_)))
        || control.memory().used() != 0
    {
        return Err(BackupError::Invalid(
            "view query limit or failed cursor retirement",
        ));
    }
    let limits = CatalogInputLimits {
        maximum_key_bytes: 1024,
        maximum_value_bytes: 8192,
    };
    let mut input = old
        .stored_view_queries(limits, &control)?
        .ok_or(BackupError::Invalid("missing persistent view input"))?;
    control.cancellation().cancel();
    if !matches!(input.advance(), Err(BackupError::Cancelled(_)))
        || !matches!(input.advance(), Err(BackupError::Finished))
        || control.memory().used() != 0
    {
        return Err(BackupError::Invalid(
            "cancelled view query retained its reader",
        ));
    }
    let independent = StorageReadControl::with_limit(64 << 10);
    let mut input = old
        .stored_view_queries(limits, &independent)?
        .ok_or(BackupError::Invalid("missing persistent view input"))?;
    capture.cancellation().cancel();
    if !matches!(input.advance(), Err(BackupError::Cancelled(_)))
        || independent.memory().used() != 0
    {
        return Err(BackupError::Invalid(
            "original backup cancellation lost at view cursor",
        ));
    }
    Ok(())
}
