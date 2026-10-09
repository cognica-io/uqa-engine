//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Binary-string byte inspection shares the ordinary and admitted scalar path.

use super::{to_i64_with_control, Produced, ProductionControl, Result, SQLError, Value};

pub(super) fn get_byte(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    control.check()?;
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("get_byte takes 2 args".into()));
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let Some(payload) = crate::expr::datums::binary_payload(&args[0], control)? else {
        return Err(SQLError::TypeMismatch(
            "get_byte requires bytea input".into(),
        ));
    };
    let bytes = payload.bytes();
    let index = to_i64_with_control(&args[1], control)?;
    let byte = usize::try_from(index)
        .ok()
        .and_then(|index| bytes.get(index))
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "2202E".into(),
            message: format!(
                "index {index} out of valid range, 0..{}",
                bytes.len() as i128 - 1
            ),
        })?;
    Ok(control.finish(Value::Int(i64::from(*byte)), control.empty_reservation())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue};

    #[test]
    fn byte_inspection_preserves_unsigned_values_bounds_and_null_precedence() {
        let token = CancellationToken::new();
        let memory = MemoryBudget::new(0);
        let control = ProductionControl::new(&memory, &token, &token);
        let data = Value::Bytes(vec![0, 128, 255]);
        for (index, expected) in [(0, 0), (1, 128), (2, 255)] {
            assert_eq!(
                *get_byte(&[data.clone(), Value::Int(index)], &control).unwrap(),
                Value::Int(expected)
            );
        }
        for (data, index, message) in [
            (data, -1, "index -1 out of valid range, 0..2"),
            (
                Value::Bytes(Vec::new()),
                0,
                "index 0 out of valid range, 0..-1",
            ),
        ] {
            let error = get_byte(&[data, Value::Int(index)], &control).unwrap_err();
            assert!(
                matches!(error, SQLError::Routine { sqlstate, message: actual } if sqlstate == "2202E" && actual == message)
            );
        }
        let unread = Value::Datum(DatumValue::new(17, 0, Vec::new()));
        assert_eq!(
            *get_byte(&[unread, Value::Null], &control).unwrap(),
            Value::Null
        );
        token.cancel();
        assert_eq!(
            get_byte(&[Value::Null, Value::Null], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
}
