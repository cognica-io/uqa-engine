//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Input = ImageDecoder<Produced<ColumnType>, SQLError>;

fn input(
    name: Arc<str>,
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
        ColumnType::from_sql_name_with_input_work(&name, &control, &mut input).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<Produced<ColumnType>>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> Result<(Produced<ColumnType>, usize), SQLError> {
    for polls in 1..100_000 {
        if let Some(value) = advance(input)? {
            return Ok((value, polls));
        }
    }
    panic!("type parsing stopped progressing")
}

#[test]
fn retained_sql_type_parsing_preserves_scalar_modifiers_aliases_and_diagnostics() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let array = |ty| ColumnType::Array(Box::new(ty));
    for (name, expected) in [
        (" _INT4 ", array(ColumnType::Integer)),
        (
            "numeric(+10,-2) [] []",
            array(array(ColumnType::Numeric {
                precision: Some(10),
                scale: Some(-2),
            })),
        ),
        (
            "TIMESTAMP(3) WITH TIME ZONE",
            ColumnType::TimestampTzPrecision(3),
        ),
        ("pg_catalog.bpchar", ColumnType::Bpchar),
        ("CHARACTER(+7)", ColumnType::Character(7)),
        (
            "\"pg_catalog\".\"varchar\"(15)[]",
            array(ColumnType::Varchar(Some(15))),
        ),
        ("PG_CATALOG._BIT(19)[]", array(array(ColumnType::Bit(None)))),
        ("bit", ColumnType::Bit(Some(1))),
        ("\"char\"", ColumnType::InternalChar),
        ("vector(+8)", ColumnType::Vector(8)),
        ("tensor(0003)", ColumnType::Tensor(3)),
    ] {
        assert_eq!(ColumnType::from_sql_name(name).unwrap(), expected, "{name}");
        let (actual, _) =
            complete(&mut input(name.into(), &native, &work, &token, &token)).unwrap();
        assert_eq!(*actual, expected, "{name}");
        assert_eq!(native.used(), actual.reserved_bytes());
        drop(actual);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
    for (name, expected) in [
        ("void[][]", "type \"void[]\" does not exist"),
        (
            "pg_node_tree [] []",
            "type \"pg_node_tree []\" does not exist",
        ),
        (
            "pg_catalog.\"TEXT\"",
            "SQL type `pg_catalog.\"TEXT\"` is not supported",
        ),
        (
            "character(-0)",
            "type mismatch: character length must be greater than zero, got -0",
        ),
        (
            "numeric(2,3,4)",
            "type mismatch: invalid numeric modifier `2,3,4`",
        ),
        ("vector(0)", "type mismatch: VECTOR requires a dimension"),
        ("time(no)", "type mismatch: invalid temporal precision: no"),
    ] {
        let ordinary = ColumnType::from_sql_name(name).unwrap_err();
        let native_error =
            complete(&mut input(name.into(), &native, &work, &token, &token)).unwrap_err();
        assert_eq!(ordinary.to_string(), expected);
        assert_eq!(native_error.to_string(), expected);
        assert_eq!(ordinary.sqlstate(), native_error.sqlstate());
        assert_eq!((native.used(), work.used()), (0, 0));
    }
    integer_modifiers(&native, &work, &token);
}

fn integer_modifiers(native: &MemoryBudget, work: &MemoryBudget, token: &CancellationToken) {
    for number in [
        "",
        "+",
        "-0",
        "+0",
        "2147483647",
        "-2147483648",
        "2147483648",
        "4294967295",
        "4294967296",
        "0000000001",
        "1_2",
        "１",
    ] {
        for unsigned in [false, true] {
            let name = if unsigned {
                format!("numeric({number},0)")
            } else {
                format!("numeric(10,{number})")
            };
            let expected = if unsigned {
                number.parse::<u32>().ok().map(|p| (p, 0))
            } else {
                number.parse::<i32>().ok().map(|s| (10, s))
            };
            let actual = complete(&mut input(name.into(), native, work, token, token));
            match expected {
                Some((precision, scale)) => assert_eq!(
                    *actual.unwrap().0,
                    ColumnType::Numeric {
                        precision: Some(precision),
                        scale: Some(scale)
                    }
                ),
                None => assert!(actual.is_err()),
            }
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn retained_sql_type_parsing_bounds_long_text_digits_and_deep_arrays() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(16 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let name: Arc<str> = format!(
                "{}NUMERIC({}10,-{}2){}{}",
                "\u{2000}".repeat(8193),
                "0".repeat(32769),
                "0".repeat(32769),
                " []".repeat(8192),
                "\u{3000}".repeat(8193)
            )
            .into();
            let (parsed, polls) =
                complete(&mut input(name.clone(), &native, &work, &token, &token)).unwrap();
            assert!(polls > 16_384);
            let ordinary = ColumnType::from_sql_name_with_control(
                &name,
                &ProductionControl::new(&native, &token, &token),
            )
            .unwrap();
            for value in [&parsed, &ordinary] {
                let mut leaf = &**value;
                let mut depth = 0;
                while let ColumnType::Array(child) = leaf {
                    depth += 1;
                    leaf = child;
                }
                assert_eq!(depth, 8192);
                assert_eq!(
                    *leaf,
                    ColumnType::Numeric {
                        precision: Some(10),
                        scale: Some(-2)
                    }
                );
            }
            assert_eq!(parsed.reserved_bytes(), ordinary.reserved_bytes());
            let retired = allocation_counter::measure(|| drop((parsed, ordinary)));
            assert_eq!(retired.count_total, 0);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_sql_type_parsing_retires_every_interrupted_or_denied_prefix() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let name: Arc<str> = format!(" NUMERIC({}10,-2){}", "0".repeat(8193), " []".repeat(16)).into();
    let (value, polls) =
        complete(&mut input(name.clone(), &native, &work, &token, &token)).unwrap();
    drop(value);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls {
        for action in 0..4 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(name.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            let held =
                (action == 3).then(|| native.reserve(native.limit() - native.used()).unwrap());
            if action == 1 || action == 2 {
                if action == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            } else if action == 3 {
                match complete(&mut reader) {
                    Ok((value, _)) => drop(value),
                    Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
                }
            }
            let retired = allocation_counter::measure(|| drop(reader));
            assert_eq!(retired.count_total, 0);
            assert_eq!(
                native.used(),
                37 + held
                    .as_ref()
                    .map_or(0, uqa_core::memory::MemoryReservation::bytes)
            );
            assert_eq!(work.used(), 0);
            drop(held);
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}
