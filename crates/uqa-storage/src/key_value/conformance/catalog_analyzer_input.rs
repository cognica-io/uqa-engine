//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field binding and label projections preserve the same private provider generation.

use super::{expect, expect_eq};
use crate::{
    catalog::{
        analyzer_input::{CatalogAnalyzerInput, CatalogAnalyzerInputStep},
        input::CatalogInputLimits,
    },
    read_control::StorageReadControl,
    PersistentStorageSession, RelationIdentity, StorageBackendError, StorageBackendResult,
};

/// Compare both physical projections with the original catalog loaders across rollback, peer commits and source retirement on a fresh disposable provider.
pub fn verify_catalog_analyzer_input(source: PersistentStorageSession) -> StorageBackendResult<()> {
    prepare(&source)?;
    let expected_bindings = source.catalog.load_table_field_analyzer_bindings()?;
    let expected_labels = source.catalog.load_table_field_analyzers()?;
    let control = StorageReadControl::with_limit(1 << 20);
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 8192,
    };
    let mut input = source.catalog.begin_analyzer_input(limits, &control)?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    later.catalog.replace_table_field_analyzer_binding(
        "public.z",
        "body",
        "search",
        "later",
        "later revision",
    )?;
    later
        .catalog
        .drop_table_field_analyzer_field("public.a_longer", "body")?;
    let owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "analyzer input retains no originating catalog handle",
    )?;
    verify_selected(&mut input, &control, &expected_bindings, &expected_labels)?;
    verify_retirement(&later, limits)
}

fn verify_selected(
    input: &mut CatalogAnalyzerInput,
    control: &StorageReadControl,
    expected_bindings: &[(String, String, String)],
    expected_labels: &[(String, String, String, String)],
) -> StorageBackendResult<()> {
    let mut bindings = Vec::new();
    let mut labels = Vec::new();
    let mut boundary = false;
    let mut complete = false;
    let mut pending = 0;
    for _ in 0..4096 {
        match input.advance()? {
            CatalogAnalyzerInputStep::Pending => pending += 1,
            CatalogAnalyzerInputStep::Binding(row) => {
                expect(!boundary, "bindings precede their complete-family boundary")?;
                bindings.push(row);
            }
            CatalogAnalyzerInputStep::BindingsComplete => {
                expect(!boundary, "binding boundary occurs once")?;
                boundary = true;
            }
            CatalogAnalyzerInputStep::Label(row) => {
                expect(boundary, "labels follow all physical bindings")?;
                labels.push(row);
            }
            CatalogAnalyzerInputStep::Complete => {
                complete = true;
                break;
            }
        }
    }
    expect(
        complete && boundary && pending > 0,
        "finite input includes deleted or absent physical slots and both boundaries",
    )?;
    let mut actual_bindings = bindings
        .iter()
        .map(|row| {
            (
                row.table.clone(),
                row.field.clone(),
                row.binding_json.clone(),
            )
        })
        .collect::<Vec<_>>();
    let mut actual_labels = labels
        .iter()
        .map(|row| {
            (
                row.table.clone(),
                row.field.clone(),
                row.phase.clone(),
                row.analyzer_name.clone(),
            )
        })
        .collect::<Vec<_>>();
    actual_bindings.sort();
    actual_labels.sort();
    expect_eq(
        &actual_bindings.as_slice(),
        &expected_bindings,
        "selected original binding bytes and membership",
    )?;
    expect_eq(
        &actual_labels.as_slice(),
        &expected_labels,
        "selected original compatibility labels and membership",
    )?;
    let bytes = bindings
        .iter()
        .map(|row| row.table.capacity() + row.field.capacity() + row.binding_json.capacity())
        .sum::<usize>()
        + labels
            .iter()
            .map(|row| {
                row.table.capacity()
                    + row.field.capacity()
                    + row.phase.capacity()
                    + row.analyzer_name.capacity()
            })
            .sum::<usize>();
    expect_eq(
        &control.memory().used(),
        &bytes,
        "completed input retains only original returned string capacities",
    )?;
    control.cancellation().cancel();
    expect(
        matches!(input.advance()?, CatalogAnalyzerInputStep::Complete),
        "completion stays idempotent after cancellation",
    )?;
    drop((bindings, labels));
    expect_eq(
        &control.memory().used(),
        &0,
        "returned row retirement releases original admission",
    )
}

fn prepare(source: &PersistentStorageSession) -> StorageBackendResult<()> {
    source.catalog.initialize_storage()?;
    source.catalog.save_schema("public")?;
    for (index, name) in ["z", "a_longer", "Table.한글"].into_iter().enumerate() {
        let id = u8::try_from(index).unwrap() + 1;
        let relation = RelationIdentity::new("public", name);
        let table = relation.qualified_name();
        source.catalog.save_table(&crate::TableSchema {
            relation,
            security: crate::RelationSecurityRow::bootstrap(),
            object_id: [id; 16],
            storage_generation: [id + 32; 16],
            analyzer_json: "{}".into(),
            fts_fields: vec![],
            vector_fields: vec![],
            columns_json: "[]".into(),
            constraints_json: "{}".into(),
        })?;
        source.catalog.replace_table_field_analyzer_binding(
            &table,
            "body",
            "both",
            "named",
            " { \"n\": 1.00, \"s\": \"한글\\u0000\" } ",
        )?;
        source
            .catalog
            .save_table_field_analyzer(&table, "legacy\0field", "INDEX", "")?;
    }
    source
        .catalog
        .replace_table_field_analyzer_binding("public.z", "deleted", "both", "removed", "{}")?;
    source.backend.begin_transaction()?;
    source
        .catalog
        .drop_table_field_analyzer_field("public.z", "deleted")?;
    source.catalog.replace_table_field_analyzer_binding(
        "public.z",
        "body",
        "search",
        "private",
        " private exact envelope ",
    )?;
    source
        .catalog
        .replace_table_field_analyzer_binding("public.z", "private", "index", "", "[]")?;
    Ok(())
}

fn next_boundary(input: &mut CatalogAnalyzerInput) -> StorageBackendResult<()> {
    for _ in 0..4096 {
        match input.advance()? {
            CatalogAnalyzerInputStep::Binding(_) | CatalogAnalyzerInputStep::Pending => (),
            CatalogAnalyzerInputStep::BindingsComplete => return Ok(()),
            _ => break,
        }
    }
    Err(StorageBackendError::Other(
        "analyzer input did not reach its binding boundary".into(),
    ))
}

fn verify_retirement(
    source: &PersistentStorageSession,
    limits: CatalogInputLimits,
) -> StorageBackendResult<()> {
    for labels in [false, true] {
        for terminal in ["cancel", "memory", "drop", "unwind"] {
            source.backend.begin_transaction()?;
            let control = StorageReadControl::with_limit(1 << 20);
            let mut input = source.catalog.begin_analyzer_input(limits, &control)?;
            source.backend.rollback_transaction()?;
            if labels {
                next_boundary(&mut input)?;
            }
            let occupied = if terminal == "memory" {
                Some(control.memory().reserve(control.memory().available())?)
            } else {
                None
            };
            if terminal == "cancel" {
                control.cancellation().cancel();
            }
            if terminal == "unwind" {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("abandoned analyzer input");
                }));
                expect(result.is_err(), "unwinding drops both input families")?;
            } else {
                if terminal != "drop" {
                    expect(
                        input.advance().is_err(),
                        "cancelled or exhausted input fails",
                    )?;
                    expect(input.advance().is_err(), "failed input cannot resume")?;
                }
                drop(input);
            }
            drop(occupied);
            expect_eq(
                &control.memory().used(),
                &0,
                "terminal input releases both provider cursors",
            )?;
            source.backend.begin_transaction()?;
            source.catalog.replace_table_field_analyzer_binding(
                "public.z",
                "body",
                "search",
                "subsequent",
                "subsequent revision",
            )?;
            source.backend.commit_transaction()?;
        }
    }
    let control = StorageReadControl::with_limit(65536);
    let mut input = source.catalog.begin_analyzer_input(
        CatalogInputLimits {
            maximum_value_bytes: 1,
            ..limits
        },
        &control,
    )?;
    for _ in 0..4096 {
        match input.advance() {
            Ok(CatalogAnalyzerInputStep::Pending) => (),
            Err(_) => {
                expect(input.advance().is_err(), "encoded value denial is terminal")?;
                return expect_eq(
                    &control.memory().used(),
                    &0,
                    "encoded value denial retires both streams",
                );
            }
            _ => break,
        }
    }
    Err(StorageBackendError::Other(
        "encoded analyzer value limit was not enforced".into(),
    ))
}
