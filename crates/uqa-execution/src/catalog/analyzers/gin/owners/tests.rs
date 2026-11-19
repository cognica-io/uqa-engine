//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{BudgetedString, MemoryBudget},
};
use uqa_storage::read_control::StorageReadControl;

fn finish<T: Send + 'static>(
    input: &mut ImageDecoder<T, StorageBackendError>,
    steps: usize,
) -> StorageBackendResult<(T, usize)> {
    for polls in 1..100_000 {
        if let Some(value) =
            input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
        {
            return Ok((value, polls));
        }
    }
    panic!("GIN ownership input stopped progressing")
}

fn text(value: &str, memory: &MemoryBudget) -> Budgeted<String> {
    let mut text = BudgetedString::new(memory);
    text.push_str(value).unwrap();
    let (text, memory) = text.into_parts();
    Budgeted::new(text, memory)
}

fn key(table: &str, field: &str, memory: &MemoryBudget) -> Budgeted<FieldKey> {
    let (table, mut lease) = text(table, memory).into_parts();
    let (field, field_memory) = text(field, memory).into_parts();
    lease.absorb(field_memory);
    Budgeted::new((table, field), lease)
}

fn build(
    entries: Vec<(FieldKey, String)>,
    control: &StorageReadControl,
) -> RetainedGinAnalyzerOwners {
    let mut decoder = ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut input = reader.input();
            let mut owners = RetainedGinAnalyzerOwners::new();
            let mut expected = GinAnalyzerOwners::new();
            for ((table, field), analyzer) in entries {
                let (table, mut memory) = input.copy_text(&table).await?.into_parts();
                let (field, field_memory) = input.copy_text(&field).await?.into_parts();
                memory.absorb(field_memory);
                let key = Budgeted::new((table, field), memory);
                let name = input.copy_text(&analyzer).await?;
                let key_pointer = owners
                    .owners
                    .get_key_value(&*key)
                    .map(|(key, _)| key.0.as_ptr().addr())
                    .unwrap_or_else(|| key.0.as_ptr().addr());
                let name_pointer = name.as_ptr().addr();
                let expected_previous = expected.insert((*key).clone(), analyzer);
                let query = (*key).clone();
                let previous = owners.insert_from(key, name, &mut input).await?;
                assert_eq!(
                    previous.as_deref().map(String::as_str),
                    expected_previous.as_deref()
                );
                let (key, name) = owners.owners.get_key_value(&query).unwrap();
                assert_eq!(key.0.as_ptr().addr(), key_pointer);
                assert_eq!(name.as_ptr().addr(), name_pointer);
            }
            for (key, name) in &expected {
                assert_eq!(
                    owners.find_from(key, &mut input).await?,
                    Some(name.as_str())
                );
            }
            Ok(owners)
        },
    )
    .unwrap();
    finish(&mut decoder, 1).unwrap().0
}

#[test]
fn retained_gin_owners_adopt_exact_buffers_and_preserve_case_sensitive_compound_keys() {
    let control = StorageReadControl::with_limit(16 << 20);
    let prefix = "relation".repeat(8192);
    let entries = (0..65)
        .flat_map(|i| {
            [
                (
                    (format!("{prefix}{i:03}"), format!("field_{i}")),
                    format!("name_{i}"),
                ),
                (
                    (format!("{prefix}{i:03}"), format!("Field_{i}")),
                    format!("case_{i}"),
                ),
                (
                    (format!("{prefix}{i:03}"), format!("field_{i}")),
                    format!("new_{i}"),
                ),
            ]
        })
        .chain([(
            ("quoted.schema".into(), "\0한글".into()),
            "\0selected".into(),
        )])
        .collect();
    let owners = build(entries, &control);
    let memory = owners
        .owners
        .iter()
        .map(|(key, name)| {
            key.0.capacity()
                + key.1.capacity()
                + name.capacity()
                + OwnedMap::<FieldKey, Budgeted<String>>::entry_bytes()
        })
        .sum::<usize>();
    assert_eq!(owners.owners.len(), 131);
    assert_eq!(control.memory().used(), memory);
    drop(owners);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_gin_lookup_uses_invoking_work_without_copying_or_reusing_the_source_allowance() {
    let source = StorageReadControl::with_limit(4 << 20);
    let table = "한".repeat(65_536);
    let field = "x".repeat(131_072);
    let owners = build(
        vec![((table.clone(), field.clone()), "name".into())],
        &source,
    );
    let retained = source.memory().used();
    let occupied = source
        .memory()
        .reserve(source.memory().available())
        .unwrap();
    source.cancellation().cancel();
    let invoking = StorageReadControl::with_limit(8192);
    let mut decoder = ImageDecoder::new(
        invoking.memory(),
        invoking.cancellation(),
        |mut reader| async move {
            let query = (table, field);
            assert_eq!(
                owners.find_from(&query, &mut reader.input()).await?,
                Some("name")
            );
            Ok(owners)
        },
    )
    .unwrap();
    let header = invoking.memory().used();
    let occupied_invoking = invoking
        .memory()
        .reserve(invoking.memory().available())
        .unwrap();
    let (owners, polls) = finish(&mut decoder, 1).unwrap();
    assert!(polls > 80);
    assert_eq!(invoking.memory().used(), occupied_invoking.bytes());
    assert!(header > 0);
    assert_eq!(source.memory().used(), retained + occupied.bytes());
    drop((owners, decoder, occupied, occupied_invoking));
    assert_eq!(source.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
}

#[test]
fn retained_gin_input_retires_original_and_provisional_owners_after_every_terminal_path() {
    for outcome in ["cancel", "memory", "zero", "drop", "unwind"] {
        let source = StorageReadControl::with_limit(4 << 20);
        let table = "table".repeat(32_768);
        let field = "field".repeat(32_768);
        let owners = build(
            vec![((table.clone(), field.clone()), "first".into())],
            &source,
        );
        let invoking = StorageReadControl::with_limit(4 << 20);
        let incoming = key(&table, &format!("{field}z"), invoking.memory());
        let analyzer = text("second", invoking.memory());
        let mut decoder = ImageDecoder::new(
            invoking.memory(),
            invoking.cancellation(),
            |mut reader| async move {
                let mut owners = owners;
                owners
                    .insert_from(incoming, analyzer, &mut reader.input())
                    .await?;
                Ok(owners)
            },
        )
        .unwrap();
        for _ in 0..8 {
            assert!(decoder
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 }
                )
                .unwrap()
                .is_none());
        }
        let occupied = (outcome == "memory").then(|| {
            invoking
                .memory()
                .reserve(invoking.memory().available())
                .unwrap()
        });
        if outcome == "cancel" {
            invoking.cancellation().cancel();
        }
        if outcome == "unwind" {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _decoder = decoder;
                    panic!("abandoned GIN ownership");
                }))
                .is_err()
            );
        } else {
            if outcome != "drop" {
                assert!(
                    finish(&mut decoder, usize::from(outcome != "zero")).is_err(),
                    "{outcome}"
                );
                assert!(decoder
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 }
                    )
                    .is_err());
            }
            drop(decoder);
        }
        drop(occupied);
        assert_eq!(source.memory().used(), 0, "{outcome}");
        assert_eq!(invoking.memory().used(), 0, "{outcome}");
    }
}
