//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::EnumTypeReference, SQLError};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Input = ImageDecoder<Produced<String>, SQLError>;

fn input(
    source: Arc<ColumnType>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        source
            .catalog_name_with_input_work(&control, &mut input)
            .await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<Produced<String>>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (Produced<String>, usize) {
    for polls in 1..100_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("catalog spelling stopped progressing")
}

#[test]
fn catalog_spelling_input_preserves_identity_modifiers_and_original_named_text() {
    for (source, expected) in [
        (ColumnType::Integer, "pg_catalog.\"int4\""),
        (ColumnType::Bpchar, "pg_catalog.\"bpchar\""),
        (ColumnType::Character(17), "pg_catalog.\"bpchar\"(17)"),
        (ColumnType::Varchar(Some(29)), "pg_catalog.\"varchar\"(29)"),
        (
            ColumnType::Numeric {
                precision: Some(19),
                scale: Some(-2),
            },
            "pg_catalog.\"numeric\"(19,-2)",
        ),
        (
            ColumnType::TimestampTzPrecision(4),
            "pg_catalog.\"timestamptz\"(4)",
        ),
        (
            ColumnType::Named("\"Mixed.型\".\"a\"\"b\"".into()),
            "\"Mixed.型\".\"a\"\"b\"",
        ),
        (
            ColumnType::Enum(EnumTypeReference {
                schema: "renamed".into(),
                name: "renamed".into(),
                oid: 42,
                array_oid: 43,
            }),
            "enum#42",
        ),
    ] {
        let source = Arc::new(ColumnType::Array(Box::new(ColumnType::Array(Box::new(
            source,
        )))));
        let expected = format!("{expected}[][]");
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
        assert_eq!(value.as_str(), expected);
        assert_eq!(source.catalog_name(), expected);
        assert!(polls >= 5);
        assert_eq!(value.reserved_bytes(), value.capacity());
        assert_eq!(native.used(), value.reserved_bytes());
        drop(value);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn catalog_spelling_input_releases_every_cancelled_prefix_and_denied_destination() {
    let source = Arc::new(ColumnType::Array(Box::new(ColumnType::Named(
        "型🙂a\"".repeat(2048),
    ))));
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    assert!(polls > 5);
    assert_eq!(value.as_str(), source.catalog_name());
    drop(value);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            }
            let allocation = allocation_counter::measure(|| drop(reader));
            assert_eq!(allocation.count_total, 0);
            assert_eq!((native.used(), work.used()), (37, 0));
        }
    }
    drop(sibling);
    let denied = MemoryBudget::new(0);
    let mut reader = input(source, &denied, &work, &token, &token);
    let error = loop {
        match advance(&mut reader) {
            Err(error) => break error,
            Ok(None) => {}
            Ok(Some(_)) => panic!("unadmitted catalog spelling"),
        }
    };
    assert_eq!(error.sqlstate(), Some("53200"));
    drop(reader);
    assert_eq!((denied.used(), work.used()), (0, 0));
}

#[test]
fn catalog_spelling_input_bounds_deep_arrays_without_recursive_formatting() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut source = ColumnType::Integer;
            for _ in 0..8192 {
                source = ColumnType::Array(Box::new(source));
            }
            let source = Arc::new(source);
            let native = MemoryBudget::new(1 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let (value, polls) =
                complete(&mut input(source.clone(), &native, &work, &token, &token));
            assert!(polls >= 2 * 8192);
            assert_eq!(
                value.as_str(),
                format!("pg_catalog.\"int4\"{}", "[]".repeat(8192))
            );
            assert_eq!(source.catalog_name(), value.as_str());
            drop((source, value));
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
