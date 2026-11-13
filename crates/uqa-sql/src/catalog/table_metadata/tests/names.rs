//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::table_metadata::SharedTableMetadata;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::{Budgeted, BudgetedString, BudgetedVec},
};

fn input(
    selected: SharedTableMetadata<Vec<String>>,
    name: &str,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<TableMetadata<Vec<String>>, ImageIoError> {
    let name = name.to_owned();
    ImageDecoder::new(memory, token, |mut reader| async move {
        selected
            .with_appended_name_from(&name, &mut reader.input())
            .await
    })
    .unwrap()
}

fn finish(
    input: &mut ImageDecoder<TableMetadata<Vec<String>>, ImageIoError>,
    steps: usize,
) -> Result<TableMetadata<Vec<String>>, ImageIoError> {
    for _ in 0..100_000 {
        if let Some(value) =
            input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
        {
            return Ok(value);
        }
    }
    panic!("table field append stopped progressing")
}

#[test]
fn admitted_field_append_preserves_prior_snapshots_exact_ownership_and_later_writes() {
    let source_memory = MemoryBudget::new(1 << 20);
    let source_token = CancellationToken::new();
    let mut source_names = BudgetedVec::new(&source_memory);
    let mut source_lease = source_memory.empty_reservation();
    for text in ["original".repeat(8192), "한글\0field".into()] {
        let mut name = BudgetedString::new(&source_memory);
        name.push_str(&text).unwrap();
        let (name, memory) = name.into_parts();
        source_lease.absorb(memory);
        source_names.push(name).unwrap();
    }
    let (source_names, source_slots) = source_names.into_parts();
    source_lease.absorb(source_slots);
    let old =
        TableMetadata::from_admitted(Budgeted::new(source_names, source_lease), &source_token)
            .unwrap();
    let selected = old.snapshot();
    let source_bytes = source_memory.used();
    let source_occupied = source_memory.reserve(source_memory.available()).unwrap();
    source_token.cancel();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut read = input(selected.clone(), "new\0name", &memory, &token);
    assert!(read
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .unwrap()
        .is_none());
    let (mut restored, visits) =
        crate::catalog::table_metadata::count_payload_visits(|| finish(&mut read, 1).unwrap());
    assert_eq!(visits, 0);
    assert_eq!(&restored[..2], selected.as_slice());
    assert_eq!(restored[2], "new\0name");
    assert!(selected.shares_definition(&old.snapshot()));
    assert!(!selected.shares_definition(&restored.snapshot()));
    let bytes = restored.capacity() * size_of::<String>()
        + restored.iter().map(String::capacity).sum::<usize>()
        + size_of::<Budgeted<Vec<String>>>();
    assert_eq!(memory.used(), bytes);
    capture_bytes(&restored, bytes);
    assert!(read
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .is_err());
    let retained = restored.snapshot();
    let occupied = memory.reserve(memory.available()).unwrap();
    token.cancel();
    TableMetadata::write(&mut restored).push("later".into());
    assert_eq!(restored.last().unwrap(), "later");
    assert_eq!(retained.len(), 3);
    assert_eq!(selected.len(), 2);
    assert_eq!(memory.used(), bytes + occupied.bytes());
    drop((restored, retained, read, occupied));
    assert_eq!(memory.used(), 0);
    assert_eq!(source_memory.used(), source_bytes + source_occupied.bytes());
    drop((old, selected, source_occupied));
    assert_eq!(source_memory.used(), 0);
}

#[test]
fn admitted_field_append_retires_partial_copies_without_changing_selected_names() {
    let old = TableMetadata::new(
        (0..257)
            .map(|i| format!("name_{i}_{}", "한".repeat(4096)))
            .collect::<Vec<_>>(),
    );
    let selected = old.snapshot();
    for outcome in ["cancel", "memory", "zero", "drop", "unwind"] {
        let memory = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let mut read = input(selected.clone(), "appended", &memory, &token);
        let initial = memory.used();
        let slots = (selected.len() + 1) * size_of::<String>();
        for _ in 0..100_000 {
            assert!(read
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 }
                )
                .unwrap()
                .is_none());
            if memory.used() > initial + slots {
                break;
            }
        }
        assert!(memory.used() > initial + slots);
        let occupied = (outcome == "memory").then(|| memory.reserve(memory.available()).unwrap());
        if outcome == "cancel" {
            token.cancel();
        }
        if outcome == "unwind" {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _read = read;
                    panic!("abandoned physical field append");
                }))
                .is_err()
            );
        } else {
            if outcome != "drop" {
                let error = finish(&mut read, usize::from(outcome != "zero")).unwrap_err();
                match outcome {
                    "cancel" => assert!(matches!(error, ImageIoError::Cancelled(_))),
                    "memory" => assert!(matches!(error, ImageIoError::Memory(_))),
                    "zero" => assert!(matches!(error, ImageIoError::Invalid)),
                    _ => unreachable!(),
                }
                assert!(read
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 }
                    )
                    .is_err());
            }
            drop(read);
        }
        drop(occupied);
        assert_eq!(memory.used(), 0);
        assert!(selected.shares_definition(&old.snapshot()));
        assert_eq!(selected.len(), 257);
    }
}
