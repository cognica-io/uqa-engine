//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{BinaryOp, RetainedInputLiteral},
    ir::{RetainedConstantValue, RetainedScalarLiteral},
};
use std::io;
use uqa_core::memory::ProductionControl;

mod domains;

fn encode(value: &Expr) -> Vec<u8> {
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let result = collect(value.image_reader(&memory, &token).unwrap());
    assert_eq!(memory.used(), 0);
    result
}

fn collect(mut reader: crate::catalog::image::DefinitionImageReader<'_>) -> Vec<u8> {
    let mut output = vec![];
    let mut buffer = [0; 127];
    loop {
        let n = reader.read(&mut buffer).unwrap();
        if n == 0 {
            return output;
        }
        output.extend_from_slice(&buffer[..n]);
    }
}

fn image(payload: &[u8]) -> Vec<u8> {
    let mut result = b"UQAEXP01\x01\0\0\0\0\0\0\0".to_vec();
    result.extend(payload);
    result
}

#[test]
fn native_scalar_input_matches_literal_wire_and_rejects_malformed_framing() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let bytes = image(&[13, 6, 14, 0, 18, 1, 2]);
    let value = Expr::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(
        *value,
        Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(Expr::UnaryMinus(Box::new(Expr::Star))),
            rhs: Box::new(Expr::IsNull {
                expr: Box::new(Expr::Default),
                negated: true
            })
        }
    );
    assert_eq!(encode(&value), bytes);
    drop(value);
    assert_eq!(memory.used(), 0);
    for length in 0..bytes.len() {
        assert!(Expr::read_image(&mut &bytes[..length], &memory, &token).is_err());
        assert_eq!(memory.used(), 0, "prefix {length}");
    }
    let mut malformed = vec![
        image(&[24]),
        image(&[255]),
        image(&[13, 10]),
        image(&[18, 2, 0]),
    ];
    let mut extra = bytes.clone();
    extra.push(0);
    malformed.push(extra);
    for count in [0u64, 2, u64::MAX] {
        let mut value = image(&[0]);
        value[8..16].copy_from_slice(&count.to_le_bytes());
        malformed.push(value);
    }
    let mut huge = image(&[10]);
    huge.extend(u64::MAX.to_le_bytes());
    malformed.push(huge);
    for bytes in malformed {
        assert!(Expr::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_scalar_input_admits_original_payloads_and_exact_peak() {
    let value = Expr::Array(vec![
        Expr::Column("original".repeat(1024).into()),
        Expr::Literal(Value::Bytes(vec![37; 4096])),
        Expr::Not(Box::new(Expr::Star)),
    ]);
    let bytes = encode(&value);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    let restored = Expr::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    let retained = memory.used();
    let peak = memory.peak();
    assert!(retained >= 4096 + 8 * 1024 + 3 * size_of::<Expr>());
    assert_eq!(*restored, value);
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    assert_eq!(memory.used(), 0);
    for limit in [0, 1, peak - 1] {
        let memory = MemoryBudget::new(limit);
        let error = Expr::read_image(&mut bytes.as_slice(), &memory, &token).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(peak);
    drop(Expr::read_image(&mut bytes.as_slice(), &memory, &token).unwrap());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_scalar_input_reconstructs_and_retires_deep_trees_on_small_stacks() {
    let depth = 10000;
    let mut bytes = image(&[]);
    for _ in 0..depth {
        bytes.extend([13, 6]);
    }
    bytes.push(0);
    bytes.extend(std::iter::repeat_n(2, depth));
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let token = CancellationToken::new();
            let memory = MemoryBudget::new(64 << 20);
            let restored = Expr::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
            assert_eq!(encode(&restored), bytes);
            drop(restored);
            assert_eq!(memory.used(), 0);
            for length in [16 + depth, 16 + 2 * depth, bytes.len() - 1] {
                assert!(Expr::read_image(&mut &bytes[..length], &memory, &token).is_err());
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[derive(Clone, Copy)]
enum Failure {
    Io,
    Cancel,
    Unwind,
}

struct Interrupted<'a> {
    bytes: &'a [u8],
    offset: usize,
    stop: usize,
    failure: Failure,
    token: &'a CancellationToken,
}
impl Read for Interrupted<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.offset >= self.stop {
            match self.failure {
                Failure::Io => return Err(io::Error::other("injected input failure")),
                Failure::Cancel => self.token.cancel(),
                Failure::Unwind => panic!("injected input unwind"),
            }
        }
        let length = output.len().min(1);
        let n = self.bytes.read(&mut output[..length])?;
        self.offset += n;
        Ok(n)
    }
}

#[test]
fn interrupted_native_scalar_input_retires_partial_native_owners() {
    let value = Expr::Array(vec![
        Expr::Literal(Value::Str("resident".repeat(127))),
        Expr::Not(Box::new(Expr::Column("pending".into()))),
    ]);
    let bytes = encode(&value);
    for failure in [Failure::Io, Failure::Cancel, Failure::Unwind] {
        for stop in [0, 16, 32, 64, bytes.len() - 1, bytes.len()] {
            let token = CancellationToken::new();
            let memory = MemoryBudget::new(16 << 20);
            let mut reader = Interrupted {
                bytes: &bytes,
                offset: 0,
                stop,
                failure,
                token: &token,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Expr::read_image(&mut reader, &memory, &token)
            }));
            match failure {
                Failure::Unwind => assert!(result.is_err()),
                Failure::Cancel => {
                    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
                }
                Failure::Io => assert!(result.unwrap().is_err()),
            }
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn decoded_retained_literals_keep_their_native_lease_after_expression_retirement() {
    let source_memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&source_memory, &token, &token);
    let constant = RetainedConstantValue::from_produced(
        control
            .copy_value(&Value::Str("resident".repeat(1024)))
            .unwrap(),
        &control,
    )
    .unwrap();
    let literal = RetainedScalarLiteral::with_control(
        constant,
        Some(
            crate::ColumnType::Text
                .clone_with_control(&control)
                .unwrap(),
        ),
        Some(control.copy_text("old.domain").unwrap()),
        Some(950),
        &control,
    )
    .unwrap();
    let source = Expr::RetainedLiteral(Box::new(RetainedInputLiteral(literal)));
    let bytes = encode(&source);
    drop(source);
    assert_eq!(source_memory.used(), 0);
    let memory = MemoryBudget::new(16 << 20);
    let value = Expr::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    let Expr::RetainedLiteral(literal) = &*value else {
        panic!("retained literal")
    };
    let survivor = literal.0.clone();
    assert_eq!(survivor.declaration.as_deref(), Some("old.domain"));
    assert_eq!(survivor.collation_oid, Some(950));
    assert_eq!(survivor.ty, Some(crate::ColumnType::Text));
    assert_eq!(encode(&value), bytes);
    drop(value);
    assert!(memory.used() >= 8 * 1024);
    assert_eq!(
        survivor.value.read().unwrap(),
        Value::Str("resident".repeat(1024))
    );
    drop(survivor);
    assert_eq!(memory.used(), 0);
}
