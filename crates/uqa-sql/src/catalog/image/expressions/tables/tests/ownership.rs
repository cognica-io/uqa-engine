//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Read;

#[test]
fn native_column_constructor_and_image_preserve_separate_literal_footprints() {
    use crate::{
        ast::RetainedInputLiteral,
        ir::{RetainedConstantValue, RetainedScalarLiteral},
    };
    use uqa_core::memory::ProductionControl;
    let source = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&source, &token, &token);
    let value = control.copy_value(&Value::Str("x".repeat(32768))).unwrap();
    let value = RetainedConstantValue::from_produced(value, &control).unwrap();
    let literal = RetainedScalarLiteral::with_control(
        value,
        Some(ColumnType::Text.clone_with_control(&control).unwrap()),
        Some(control.copy_text("pg_catalog.text").unwrap()),
        Some(100),
        &control,
    )
    .unwrap();
    let mut column = ColumnDef::nullable("v", ColumnType::Text);
    column.default = Some(Expr::RetainedLiteral(Box::new(RetainedInputLiteral(
        literal,
    ))));
    let columns = vec![column];
    let destination = MemoryBudget::new(1 << 20);
    let (native, native_children) = crate::catalog::definition_capture::foreign::stored_columns(
        &columns,
        &ProductionControl::new(&destination, &token, &token),
    )
    .unwrap();
    assert!(native_children > 32768);
    assert_eq!(
        destination.used(),
        native.reserved_bytes() + native_children
    );
    let bytes = encode(&native, 127);
    drop(columns);
    assert_eq!(source.used(), 0);
    let restored_memory = MemoryBudget::new(1 << 20);
    let (restored, restored_children) = crate::catalog::image::read_columns_image_with_footprint(
        &mut bytes.as_slice(),
        &restored_memory,
        &token,
    )
    .unwrap();
    assert_eq!(restored_children, native_children);
    assert_eq!(
        restored_memory.used(),
        restored.reserved_bytes() + restored_children
    );
    assert_eq!(encode(&restored, 31), bytes);
    drop((native, restored));
    assert_eq!(destination.used(), 0);
    assert_eq!(restored_memory.used(), 0);
}

#[test]
fn native_column_input_rejects_partial_invalid_and_unfunded_images() {
    let bytes = encode(&[complete()], 127);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for length in 0..bytes.len() {
        assert!(read_columns_image(&mut &bytes[..length], &memory, &token).is_err());
        assert_eq!(memory.used(), 0, "prefix {length}");
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(read_columns_image(&mut trailing.as_slice(), &memory, &token).is_err());
    let mut hostile = bytes.clone();
    hostile[8..16].fill(255);
    assert!(read_columns_image(&mut hostile.as_slice(), &memory, &token).is_err());
    let literal = literal().1;
    for position in [0, 25, 30, 47, 50, 66, 67] {
        let mut bytes = literal.clone();
        bytes[position] = 255;
        assert!(read_columns_image(&mut bytes.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    let ample = MemoryBudget::new(1 << 20);
    drop(read_columns_image(&mut bytes.as_slice(), &ample, &token).unwrap());
    let peak = ample.peak();
    for limit in [0, 1, peak - 1] {
        let denied = MemoryBudget::new(limit);
        assert_eq!(
            read_columns_image(&mut bytes.as_slice(), &denied, &token)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(denied.used(), 0);
    }
    let exact = MemoryBudget::new(peak);
    drop(read_columns_image(&mut bytes.as_slice(), &exact, &token).unwrap());
    assert_eq!(exact.used(), 0);
}

#[test]
fn native_column_input_retires_read_errors_cancellation_and_unwinding() {
    let bytes = encode(&[complete()], 127);
    let memory = MemoryBudget::new(1 << 20);
    for failure in 0..3 {
        for stop in [17, 100, bytes.len() / 2, bytes.len()] {
            let token = CancellationToken::new();
            let mut source = Interrupted {
                bytes: &bytes,
                offset: 0,
                stop,
                failure,
                token: &token,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                read_columns_image(&mut source, &memory, &token)
            }));
            assert!(result.is_err() || result.unwrap().is_err());
            assert_eq!(memory.used(), 0);
        }
    }
}

struct Interrupted<'a> {
    bytes: &'a [u8],
    offset: usize,
    stop: usize,
    failure: u8,
    token: &'a CancellationToken,
}
impl Read for Interrupted<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.offset >= self.stop {
            match self.failure {
                0 => return Err(std::io::Error::other("injected column input error")),
                1 => self.token.cancel(),
                _ => panic!("injected column input unwind"),
            }
        }
        let count = output
            .len()
            .min(self.bytes.len() - self.offset)
            .min(self.stop.saturating_sub(self.offset).max(1));
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

#[test]
fn native_column_output_keeps_scratch_independent_of_attribute_payload_size() {
    let mut column = ColumnDef::nullable("v", ColumnType::Text);
    column.check_name = Some("original".repeat(1 << 20));
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let columns = [column];
    let mut buffer = vec![0; 20000];
    let mut length = 0;
    let allocations = allocation_counter::measure(|| {
        let mut reader = columns_image_reader(&columns, &memory, &token).unwrap();
        assert_eq!(memory.used(), 0);
        loop {
            let count = reader.read(&mut buffer).unwrap();
            assert!(count <= 16384);
            if count == 0 {
                break;
            }
            length += count;
        }
    });
    assert!(length > 8 << 20);
    assert_eq!(allocations.bytes_current, 0);
    assert!(allocations.bytes_total < 16384);
    assert_eq!(memory.used(), 0);
    let zero = MemoryBudget::new(0);
    let mut reader = columns_image_reader(&columns, &zero, &token).unwrap();
    assert_eq!(
        reader.read(&mut buffer).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert!(reader.read(&mut []).is_err());
    assert_eq!(zero.used(), 0);
    let token = CancellationToken::new();
    let mut reader = columns_image_reader(&columns, &memory, &token).unwrap();
    reader.read(&mut [0; 100]).unwrap();
    assert!(memory.used() > 0);
    token.cancel();
    assert!(reader.read(&mut buffer).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_column_images_reconstruct_deep_defaults_without_recursive_retirement() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let mut expression = Expr::Star;
            for _ in 0..10000 {
                expression = Expr::Not(Box::new(expression));
            }
            let mut column = ColumnDef::nullable("v", ColumnType::Integer);
            column.default = Some(expression);
            let bytes = encode(&[column], 127);
            let memory = MemoryBudget::new(64 << 20);
            let decoded =
                read_columns_image(&mut bytes.as_slice(), &memory, &CancellationToken::new())
                    .unwrap();
            assert_eq!(encode(&decoded, 31), bytes);
            drop(decoded);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
