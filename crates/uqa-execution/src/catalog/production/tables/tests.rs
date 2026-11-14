//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::Budgeted,
};
use uqa_sql::catalog::table_metadata::TableMetadata;

type Source = Arc<Vec<(RelationIdentity, CatalogTableSnapshot)>>;
type Input = ImageDecoder<Tables, SQLError>;

fn selected_table(
    (key, table): &(RelationIdentity, CatalogTableSnapshot),
) -> (&RelationIdentity, CatalogTableSnapshot) {
    (key, table.clone())
}

fn source(memory: &MemoryBudget) -> Source {
    let token = CancellationToken::new();
    let array_name = "wide_array_".repeat(4096);
    let memory = memory.reserve(array_name.capacity()).unwrap();
    let name =
        TableMetadata::from_admitted(Budgeted::new(Some(array_name), memory), &token).unwrap();
    let prefix = "同".repeat(3000);
    Arc::new(
        [
            ("schema", "b"),
            ("schema", "a"),
            ("public", "a"),
            ("schema", "b"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (schema, name_suffix))| {
            let mut table = crate::catalog::test_support::table_snapshot(
                [u8::try_from(index + 1).unwrap(); 16],
                Vec::new(),
                uqa_sql::ast::TableConstraintSet::default(),
            );
            table.row_type_array_name = name.snapshot();
            (
                RelationIdentity::new(
                    format!("{prefix}{schema}"),
                    format!("{prefix}{name_suffix}"),
                ),
                table,
            )
        })
        .collect(),
    )
}

fn input(
    source: Source,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        CatalogReadSnapshot::capture_tables_from_input(
            source.iter().map(selected_table),
            &native,
            &original,
            &mut input,
        )
        .await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<Tables>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (Tables, usize) {
    for polls in 1..100_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("catalog table capture stopped progressing")
}

#[test]
fn retained_table_capture_preserves_original_order_replacement_and_shared_metadata_ownership() {
    let source_memory = MemoryBudget::new(1 << 20);
    let source = source(&source_memory);
    let expected = source
        .iter()
        .map(|(key, value)| ((key.schema.as_str(), key.name.as_str()), value.object_id))
        .collect::<BTreeMap<_, _>>();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let mut ordinary = None;
    let allocations = allocation_counter::measure(|| {
        ordinary = Some(
            CatalogReadSnapshot::capture_tables_with_control(
                source.iter().map(|(key, table)| (key, table.clone())),
                &control,
            )
            .unwrap(),
        );
    });
    let ordinary = ordinary.unwrap();
    assert_eq!(
        allocations.bytes_current,
        i64::try_from(native.used()).unwrap()
    );
    let ordinary_bytes = native.used();
    let (retained, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    assert!(polls > 50);
    assert_eq!(native.used(), ordinary_bytes * 2);
    let actual = retained
        .iter()
        .map(|(key, value)| ((key.schema.as_str(), key.name.as_str()), value.object_id))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual, expected);
    drop((actual, expected));
    assert_eq!(
        retained.keys().collect::<Vec<_>>(),
        ordinary.keys().collect::<Vec<_>>()
    );
    for table in retained.values() {
        assert!(table
            .row_type_array_name
            .shares_definition(&source[0].1.row_type_array_name));
    }
    assert_eq!(work.used(), 0);
    drop((ordinary, source));
    assert!(source_memory.used() > 40_000);
    drop(retained);
    assert_eq!((native.used(), source_memory.used()), (0, 0));
}

#[test]
fn retained_table_capture_retires_all_pending_prefixes_without_releasing_selected_metadata() {
    let source_memory = MemoryBudget::new(1 << 20);
    let source = source(&source_memory);
    let selected_bytes = source_memory.used();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let (tables, polls) = complete(&mut input(
        source.clone(),
        &native,
        &work,
        &original,
        &invoking,
    ));
    drop(tables);
    let sibling = native.reserve(13).unwrap();
    for prefix in 0..polls - 1 {
        for cancelled in [None, Some(&original), Some(&invoking)] {
            let mut selected = input(source.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut selected).unwrap().is_none());
            }
            if let Some(token) = cancelled {
                token.cancel();
                assert_eq!(
                    advance(&mut selected).err().unwrap().sqlstate(),
                    Some("57014")
                );
                token.reset();
                assert_eq!(
                    advance(&mut selected).err().unwrap().sqlstate(),
                    Some("XX001")
                );
            }
            drop(selected);
            assert_eq!(
                (native.used(), work.used(), source_memory.used()),
                (13, 0, selected_bytes)
            );
        }
    }
    drop((source, sibling));
    assert_eq!((native.used(), source_memory.used()), (0, 0));
}

#[test]
fn retained_table_capture_denies_native_growth_and_preserves_empty_catalogs() {
    let source_memory = MemoryBudget::new(1 << 20);
    let source = source(&source_memory);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for limit in [0, 1, 128, 1024, 32_768] {
        let native = MemoryBudget::new(limit);
        let mut selected = input(source.clone(), &native, &work, &token, &token);
        let mut error = None;
        for _ in 0..100_000 {
            match advance(&mut selected) {
                Ok(None) => {}
                Ok(Some(_)) => panic!("unadmitted table capture"),
                Err(value) => {
                    error = Some(value);
                    break;
                }
            }
        }
        assert_eq!(error.unwrap().sqlstate(), Some("53200"));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
    let native = MemoryBudget::new(0);
    let (empty, _) = complete(&mut input(
        Arc::new(Vec::new()),
        &native,
        &work,
        &token,
        &token,
    ));
    assert!(empty.is_empty());
    assert_eq!((native.used(), work.used()), (0, 0));
}
