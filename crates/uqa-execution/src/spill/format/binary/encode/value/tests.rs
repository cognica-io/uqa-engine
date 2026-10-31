//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn constant_record_encoding_admits_buffers_and_decimal_workspace_before_construction() {
    let value = Value::List(vec![
        Value::Str("λ🙂".repeat(256)),
        Value::Decimal(
            uqa_core::DecimalValue::parse("123456789012345678901234567890.00100").unwrap(),
        ),
        Value::Float(-0.0),
    ]);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut record = None;
    let measured = allocation_counter::measure(|| {
        record = Some(encode_constant_record_with_control(&value, &control).unwrap());
    });
    let record = record.unwrap();
    assert_eq!(budget.used(), record.capacity());
    assert_eq!(record.reserved_bytes(), record.capacity());
    assert_eq!(measured.bytes_current, budget.used() as i64);
    let decoded = crate::spill::format::decode_constant_record(&record, &control).unwrap();
    assert!(decoded.has_same_representation(&value));
    drop(decoded);
    drop(record);
    assert_eq!(budget.used(), 0);
    let mut succeeded = false;
    for available in [0, 1, 8, 32, 128, 512, 2048, 1 << 20] {
        let memory = MemoryBudget::new(available);
        let control = ProductionControl::new(&memory, &token, &token);
        match encode_constant_record_with_control(&value, &control) {
            Ok(record) => {
                succeeded = true;
                drop(record);
            }
            Err(error) => assert_eq!(
                crate::physical::physical_exec_error(error).sqlstate(),
                Some("53200")
            ),
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(succeeded);
}

#[test]
fn constant_record_encoding_rejects_large_payloads_before_allocation_for_quota_or_cancellation() {
    let value = Value::Str("x".repeat(1 << 22));
    for cancelled in [None, Some(false), Some(true)] {
        let memory = MemoryBudget::new(if cancelled.is_none() { 0 } else { usize::MAX });
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if let Some(cancel_original) = cancelled {
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        if cancelled.is_some() {
            assert!(control.check().is_err());
        }
        let diagnostic = allocation_counter::measure(|| {
            let error = control
                .check()
                .and_then(|()| control.reserve(size_of::<u64>()).map(|_| ()))
                .unwrap_err();
            let error = crate::ExecError::from(error);
            assert_eq!(
                crate::physical::physical_exec_error(error).sqlstate(),
                Some(if cancelled.is_some() {
                    "57014"
                } else {
                    "53200"
                })
            );
        });
        let measured = allocation_counter::measure(|| {
            let error = encode_constant_record_with_control(&value, &control).unwrap_err();
            assert_eq!(
                crate::physical::physical_exec_error(error).sqlstate(),
                Some(if cancelled.is_some() {
                    "57014"
                } else {
                    "53200"
                })
            );
        });
        // The typed SQL error formats its diagnostic independently of the denied record. No record allocation may add to that exact diagnostic-only baseline.
        assert_eq!(measured.count_total, diagnostic.count_total);
        assert_eq!(measured.bytes_total, diagnostic.bytes_total);
        assert_eq!(memory.used(), 0);
    }
}
