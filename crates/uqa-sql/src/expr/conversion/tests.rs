//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn container_output_keeps_bpchar_padding_while_scalar_text_casts_trim() {
    for field in [
        Value::FixedChar("a   ".into()),
        Value::Datum(uqa_core::DatumValue::new(
            1042,
            0,
            vec![11, b'a', b' ', b' ', b' '],
        )),
    ] {
        assert_eq!(value_to_string(&field).unwrap(), "a");
        let row = Value::Row(vec![field.clone()].into());
        let record = Value::Record(vec![("value".into(), field.clone())].into());
        let array = Value::Array(ArrayValue::try_new(vec![field]).unwrap());
        let nested = Value::Row(vec![array.clone()].into());
        for (value, expected) in [
            (row, r#"("a   ")"#),
            (record, r#"("a   ")"#),
            (array, r#"{"a   "}"#),
            (nested, r#"("{""a   ""}")"#),
        ] {
            assert_eq!(value_to_string(&value).unwrap(), expected);
        }
    }
}

#[test]
fn padded_container_output_keeps_memory_and_cancellation_limits() {
    let memory = MemoryBudget::new(256);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let value = Value::Row(vec![Value::FixedChar(" ".repeat(1024))].into());
    assert_eq!(
        value_to_string_with_control(&value, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert_eq!(
        value_to_string_with_control(&value, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), 0);
}
