//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::RangeAlignment, SQLError};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Input = ImageDecoder<Produced<ColumnType>, SQLError>;

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
        source.clone_from_input(&control, &mut input).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<Produced<ColumnType>>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (Produced<ColumnType>, usize) {
    for polls in 1..100_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("type copy stopped progressing")
}

fn references() -> Vec<ColumnType> {
    let schema = "schema_型🦀\"".repeat(900);
    let name = "value_é.型".repeat(900);
    let range = RangeTypeReference {
        schema: schema.clone(),
        name: name.clone(),
        oid: 17,
        array_oid: 29,
        alignment: RangeAlignment::Double,
    };
    vec![
        ColumnType::Named(name.clone()),
        ColumnType::Domain {
            schema: schema.clone(),
            name: name.clone(),
            oid: 37,
            array_oid: Some(41),
            collation_oid: Some(950),
            base: Box::new(ColumnType::Array(Box::new(ColumnType::Numeric {
                precision: Some(19),
                scale: Some(-2),
            }))),
        },
        ColumnType::Enum(EnumTypeReference {
            schema: schema.clone(),
            name: name.clone(),
            oid: 43,
            array_oid: 47,
        }),
        ColumnType::Composite(CompositeTypeReference {
            schema,
            name,
            oid: 53,
            array_oid: 59,
            relation_oid: 61,
        }),
        ColumnType::DeclaredRange(range.clone()),
        ColumnType::DeclaredMultirange(range),
    ]
}

#[test]
fn retained_type_copy_preserves_complete_metadata_and_native_payload_ownership() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for ty in references() {
        let expected = serde_json::to_value(&ty).unwrap();
        let source = Arc::new(ty);
        let weak = Arc::downgrade(&source);
        let control = ProductionControl::new(&native, &token, &token);
        let ordinary = source.clone_with_control(&control).unwrap();
        let ordinary_bytes = native.used();
        let mut reader = input(source, &native, &work, &token, &token);
        assert!(weak.upgrade().is_some());
        let (value, polls) = complete(&mut reader);
        assert!(polls > 3);
        assert!(weak.upgrade().is_none());
        assert_eq!(serde_json::to_value(&*value).unwrap(), expected);
        assert_eq!(serde_json::to_value(&*ordinary).unwrap(), expected);
        assert_eq!(value.reserved_bytes(), ordinary_bytes);
        assert_eq!(native.used(), ordinary_bytes * 2);
        assert_eq!(work.used(), 0);
        drop((value, ordinary));
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_type_copy_retires_each_suspended_prefix_and_both_cancellation_owners() {
    let source = Arc::new(references().remove(1));
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(value);
    let sibling = native.reserve(23).unwrap();
    for prefix in 0..polls - 1 {
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
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("XX001"));
            }
            drop(reader);
            assert_eq!((native.used(), work.used()), (23, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_type_copy_admits_payloads_before_construction_and_keeps_scalar_zero_allowance() {
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for ty in [
        ColumnType::Integer,
        ColumnType::Varchar(Some(53)),
        ColumnType::Numeric {
            precision: Some(17),
            scale: Some(-3),
        },
    ] {
        let source = Arc::new(ty);
        let native = MemoryBudget::new(0);
        let (value, _) = complete(&mut input(source.clone(), &native, &work, &token, &token));
        assert_eq!(*value, *source);
        assert_eq!(value.reserved_bytes(), 0);
    }
    let source = Arc::new(references().remove(1));
    for available in [0, 1, 64, 8192, 16384, 22000] {
        let native = MemoryBudget::new(available);
        let reader = input(source.clone(), &native, &work, &token, &token);
        let error = reader.read(&mut std::io::empty()).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((native.used(), work.used()), (0, 0));
        assert!(native.peak() <= available);
    }
}

#[test]
fn retained_deep_type_copy_uses_constant_stack_and_retires_partial_chains() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut ty = ColumnType::Character(17);
            for index in 0..10_000 {
                ty = if index % 2 == 0 {
                    ColumnType::Domain {
                        schema: "public".into(),
                        name: "nested".into(),
                        oid: 70_000 + index,
                        array_oid: None,
                        collation_oid: Some(950),
                        base: Box::new(ty),
                    }
                } else {
                    ColumnType::Array(Box::new(ty))
                };
            }
            let source = Arc::new(ty);
            let native = MemoryBudget::new(4 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let (value, polls) =
                complete(&mut input(source.clone(), &native, &work, &token, &token));
            assert!(polls > 10_000);
            let mut actual = &*value;
            for index in (0..10_000).rev() {
                actual = match actual {
                    ColumnType::Array(child) => {
                        assert_eq!(index % 2, 1);
                        child
                    }
                    ColumnType::Domain {
                        schema,
                        name,
                        oid,
                        collation_oid,
                        base,
                        ..
                    } => {
                        assert_eq!(index % 2, 0);
                        assert_eq!(
                            (schema.as_str(), name.as_str(), *oid, *collation_oid),
                            ("public", "nested", 70_000 + index, Some(950))
                        );
                        base
                    }
                    _ => panic!("type chain ended early"),
                };
            }
            assert_eq!(*actual, ColumnType::Character(17));
            assert_eq!(work.used(), 0);
            drop(value);
            assert_eq!(native.used(), 0);
            for prefix in [1, 100, 10_000] {
                let mut reader = input(source.clone(), &native, &work, &token, &token);
                for _ in 0..prefix {
                    assert!(advance(&mut reader).unwrap().is_none());
                }
                drop(reader);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
