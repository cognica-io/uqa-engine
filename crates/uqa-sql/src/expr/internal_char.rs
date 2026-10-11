//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`'s internal char is one unsigned byte, with signed integer conversion and octal high-byte output.

use uqa_core::{
    memory::{Produced, ProductionControl},
    Value, ValueRetentionError,
};

pub(crate) fn input_byte(text: &str) -> u8 {
    let bytes = text.as_bytes();
    match bytes {
        [b'\\', a @ b'0'..=b'7', b @ b'0'..=b'7', c @ b'0'..=b'7'] => {
            ((u16::from(*a - b'0') << 6) | (u16::from(*b - b'0') << 3) | u16::from(*c - b'0')) as u8
        }
        bytes => bytes.first().copied().unwrap_or(0),
    }
}

pub(crate) fn value_byte(value: &Value) -> Option<u8> {
    let Value::Str(text) = value else {
        return None;
    };
    if text.is_empty() {
        return Some(0);
    }
    let mut characters = text.chars();
    let byte = u8::try_from(u32::from(characters.next()?)).ok()?;
    characters.next().is_none().then_some(byte)
}

pub(crate) fn carrier_with_control(
    byte: u8,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, ValueRetentionError> {
    let text = if byte == 0 {
        control.copy_text("")?
    } else {
        control.format(format_args!("{}", char::from(byte)))?
    };
    let (text, memory) = text.into_parts();
    control.finish(Value::Str(text), memory)
}

pub(crate) fn output_with_control(
    byte: u8,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    if byte >= 128 {
        control.format(format_args!("\\{byte:03o}"))
    } else if byte == 0 {
        control.copy_text("")
    } else {
        control.format(format_args!("{}", char::from(byte)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        expr::{cast_value_from, composites::datum, datums},
        ColumnType,
    };
    use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue};

    #[test]
    fn input_and_output_preserve_postgresql_byte_and_integer_rules() {
        for (text, byte, output) in [
            ("", 0, ""),
            ("abc", b'a', "a"),
            ("é", 0xc3, "\\303"),
            ("\\377", 255, "\\377"),
            ("\\777", 255, "\\377"),
            ("\\128", b'\\', "\\"),
        ] {
            assert_eq!(input_byte(text), byte);
            let value = cast_value_from(
                &Value::Str(text.into()),
                "pg_catalog.\"char\"",
                Some("pg_catalog.\"text\""),
            )
            .unwrap();
            assert_eq!(value_byte(&value), Some(byte));
            assert_eq!(
                cast_value_from(&value, "pg_catalog.\"text\"", Some("pg_catalog.\"char\""))
                    .unwrap(),
                Value::Str(output.into())
            );
            assert_eq!(
                crate::result::format_postgres_text(&value, &ColumnType::InternalChar, None)
                    .unwrap(),
                output
            );
        }
        for integer in -128_i64..=127 {
            let value = cast_value_from(
                &Value::Int(integer),
                "pg_catalog.\"char\"",
                Some("pg_catalog.\"int4\""),
            )
            .unwrap();
            assert_eq!(value_byte(&value), Some(integer as u8));
            assert_eq!(
                cast_value_from(&value, "pg_catalog.\"int4\"", Some("pg_catalog.\"char\""))
                    .unwrap(),
                Value::Int(integer)
            );
        }
        for integer in [-129, 128] {
            let error = cast_value_from(
                &Value::Int(integer),
                "pg_catalog.\"char\"",
                Some("pg_catalog.\"int4\""),
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("22003"));
            assert_eq!(error.to_string(), "\"char\" out of range");
        }
    }

    #[test]
    fn retained_bytes_and_carrier_order_preserve_every_unsigned_byte() {
        let token = CancellationToken::new();
        let memory = MemoryBudget::new(4096);
        let control = ProductionControl::new(&memory, &token, &token);
        let mut previous: Option<Value> = None;
        for byte in 0..=255 {
            let value =
                datums::read_with_control(&DatumValue::new(18, 0, vec![byte]), &control).unwrap();
            assert_eq!(value_byte(&value), Some(byte));
            assert_eq!(
                datum::encode_bits(&value, &ColumnType::InternalChar),
                Some(u64::from(byte))
            );
            assert_eq!(
                datum::decode_bits(u64::from(byte), &ColumnType::InternalChar),
                Some((*value).clone())
            );
            let encoded = serde_json::to_string(&*value).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), *value);
            if let Some(previous) = &previous {
                assert!(previous < &*value);
            }
            previous = Some((*value).clone());
            assert_eq!(memory.used(), value.reserved_bytes());
            drop(value);
            assert_eq!(memory.used(), 0);
        }
    }

    #[test]
    fn character_reads_and_output_admit_memory_and_both_cancellation_tokens() {
        let datum = DatumValue::new(18, 0, vec![255]);
        let token = CancellationToken::new();
        let memory = MemoryBudget::new(0);
        let control = ProductionControl::new(&memory, &token, &token);
        assert_eq!(
            datums::read_with_control(&datum, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(
            crate::SQLError::from(output_with_control(255, &control).unwrap_err()).sqlstate(),
            Some("53200")
        );
        assert_eq!(memory.used(), 0);
        let memory = MemoryBudget::new(4096);
        for original_cancelled in [false, true] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let control = ProductionControl::new(&memory, &original, &invoking);
            assert_eq!(
                datums::read_with_control(&datum, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(
                crate::SQLError::from(output_with_control(255, &control).unwrap_err()).sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), 0);
        }
    }
}
