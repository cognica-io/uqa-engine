//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Lookup<'a> {
    value: Value,
    cancellation: Option<&'a CancellationToken>,
}

impl RowLookup for Lookup<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        if let Some(cancellation) = self.cancellation {
            cancellation.cancel();
        }
        (name == "value").then_some(&self.value)
    }
    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value> {
        if qualifier == "table" {
            self.column(column)
        } else {
            None
        }
    }
    fn column_is_ambiguous(&self, name: &str) -> bool {
        name == "ambiguous"
    }
    fn qualified_column_is_ambiguous(&self, _: &str, column: &str) -> bool {
        column == "ambiguous"
    }
}

#[test]
fn controlled_column_lookup_keeps_missing_and_ambiguous_slot_semantics() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let row = Lookup {
        value: Value::Str("payload".repeat(32)),
        cancellation: None,
    };
    let context = EvalContext::from_row_lookup(&row, &[]);
    for qualified in [false, true] {
        let read = |name| {
            if qualified {
                context.qualified_column_value_with_control("table", name, &control)
            } else {
                context.column_value_with_control(name, &control)
            }
        };
        let value = read("value").unwrap();
        assert_eq!(*value, row.value);
        assert!(value.reserved_bytes() >= 224);
        assert_eq!(budget.used(), value.reserved_bytes());
        drop(value);
        assert_eq!(*read("absent").unwrap(), Value::Null);
        assert_eq!(read("ambiguous").unwrap_err().sqlstate(), Some("42702"));
        assert_eq!(budget.used(), 0);
    }
    let empty = MemoryBudget::new(0);
    let control = ProductionControl::new(&empty, &token, &token);
    assert_eq!(
        context
            .column_value_with_control("value", &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(empty.used(), 0);
}

#[test]
fn column_lookup_cancellation_after_borrowed_access_prevents_output_production() {
    let budget = MemoryBudget::new(4096);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let token = if original_cancelled {
            &original
        } else {
            &invoking
        };
        let row = Lookup {
            value: Value::Str("borrowed".repeat(32)),
            cancellation: Some(token),
        };
        let context = EvalContext::from_row_lookup(&row, &[]);
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            context
                .column_value_with_control("value", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
