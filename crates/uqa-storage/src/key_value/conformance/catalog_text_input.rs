//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native providers retain the same text rows across discarded private branches.

use super::{expect, expect_eq};
use crate::{
    catalog::text_input::{
        CatalogTextFamily, CatalogTextInput, CatalogTextLimits, CatalogTextRow, CatalogTextStep,
    },
    read_control::StorageReadControl,
    PersistentStorageSession, StorageBackendResult, StorageSavepointId,
};

use uqa_core::memory::Budgeted;

type Inputs = Vec<(CatalogTextFamily, CatalogTextInput)>;

/// Verify a fresh disposable session's native text input, including original private rows, later commits, source retirement and encoded-value admission.
pub fn verify_catalog_text_input(source: PersistentStorageSession) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let limits = CatalogTextLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 8192,
    };
    let (mut inputs, later) = prepare(source, limits, &control)?;
    let mut retained = Vec::new();
    for (family, input) in &mut inputs {
        verify_family(*family, input, &mut retained)?;
    }
    expect_eq(
        &control.memory().used(),
        &retained
            .iter()
            .map(|row| row.name.capacity() + row.value.capacity())
            .sum::<usize>(),
        "completed text input retires cursor headers and retains only returned native text",
    )?;
    control.cancellation().cancel();
    for (_, input) in &mut inputs {
        expect(
            matches!(input.advance()?, CatalogTextStep::Complete),
            "completed input does not reopen its provider",
        )?;
    }
    drop(retained);
    expect_eq(
        &control.memory().used(),
        &0,
        "last text owner releases its original allowance",
    )?;

    let limited = StorageReadControl::with_limit(65536);
    let mut input = later.catalog.begin_text_input(
        CatalogTextFamily::Models,
        CatalogTextLimits {
            maximum_value_bytes: 1,
            ..limits
        },
        &limited,
    )?;
    expect(
        input.advance().is_err(),
        "encoded value limit precedes text decoding",
    )?;
    expect(
        input.advance().is_err(),
        "failed catalog input cannot resume",
    )?;
    expect_eq(
        &limited.memory().used(),
        &0,
        "failed input releases provider and cursor memory",
    )?;
    Ok(())
}

fn prepare(
    source: PersistentStorageSession,
    limits: CatalogTextLimits,
    control: &StorageReadControl,
) -> StorageBackendResult<(Inputs, PersistentStorageSession)> {
    use CatalogTextFamily::{
        AnalyzerDescriptors, Analyzers, Metadata, Models, PathIndexes, ScoringParameters,
    };
    let name = "catalog-input\0한글";
    let original = "{ \"n\": 1.00, \"text\": \"exact\\u0000\" }";
    let private = "{ \"n\": -0.0, \"text\": \"private\" }";
    source.catalog.initialize_storage()?;
    source.catalog.set_metadata(name, original)?;
    source.catalog.save_model(name, original)?;
    source
        .catalog
        .save_model("catalog-input-deleted", original)?;
    source.catalog.save_scoring_params(name, original)?;
    source
        .catalog
        .save_analyzer_revision(name, original, private)?;
    source.catalog.save_analyzer("catalog-input-legacy", "{}")?;
    source.catalog.save_named_graph("catalog-input-graph")?;
    source
        .catalog
        .save_path_index("catalog-input-graph", "[[\"edge\"]]")?;

    source.backend.begin_transaction()?;
    let savepoint = StorageSavepointId::allocate();
    source.backend.savepoint(savepoint)?;
    source.catalog.save_model(name, private)?;
    source
        .catalog
        .save_model("catalog-input-private", original)?;
    source.catalog.drop_model("catalog-input-deleted")?;
    let inputs = [
        Metadata,
        Models,
        ScoringParameters,
        Analyzers,
        AnalyzerDescriptors,
        PathIndexes,
    ]
    .into_iter()
    .map(|family| {
        source
            .catalog
            .begin_text_input(family, limits, control)
            .map(|input| (family, input))
    })
    .collect::<StorageBackendResult<Vec<_>>>()?;

    source.backend.rollback_to_savepoint(savepoint)?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    later.catalog.save_model(name, "later")?;
    later.catalog.save_model("catalog-input-deleted", "later")?;
    later.catalog.set_metadata(name, "later")?;
    let catalog_owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        catalog_owner.upgrade().is_none(),
        "text input does not retain the source catalog handle",
    )?;
    Ok((inputs, later))
}

fn verify_family(
    family: CatalogTextFamily,
    input: &mut CatalogTextInput,
    retained: &mut Vec<Budgeted<CatalogTextRow>>,
) -> StorageBackendResult<()> {
    use CatalogTextFamily::{AnalyzerDescriptors, Models, PathIndexes};
    let name = "catalog-input\0한글";
    let original = "{ \"n\": 1.00, \"text\": \"exact\\u0000\" }";
    let private = "{ \"n\": -0.0, \"text\": \"private\" }";
    let mut found = std::collections::BTreeMap::new();
    let mut pending = 0;
    let mut calls = 0;
    loop {
        calls += 1;
        expect(
            calls < 1024,
            "finite fixture text input completes without replay",
        )?;
        match input.advance()? {
            CatalogTextStep::Pending => pending += 1,
            CatalogTextStep::Row(row) => {
                found.insert(row.name.clone(), row.value.clone());
                retained.push(row);
            }
            CatalogTextStep::Complete => break,
        }
    }
    let expected = match family {
        Models | AnalyzerDescriptors => private,
        PathIndexes => "[[\"edge\"]]",
        _ => original,
    };
    let selected = if matches!(family, PathIndexes) {
        "catalog-input-graph"
    } else {
        name
    };
    expect_eq(
        &found.get(selected).map(String::as_str),
        &Some(expected),
        "retained text preserves the original complete native value",
    )?;
    if matches!(family, Models) {
        expect_eq(
            &found.len(),
            &2,
            "only the original private model membership is exposed",
        )?;
        expect_eq(
            &found.get("catalog-input-private").map(String::as_str),
            &Some(original),
            "discarded private insertion stays visible",
        )?;
        expect(
            pending > 0,
            "private tombstone consumes a separate input transition",
        )?;
    }
    if matches!(family, AnalyzerDescriptors) {
        expect(
            !found.contains_key("catalog-input-legacy"),
            "absent legacy descriptor remains absent",
        )?;
    }
    Ok(())
}
