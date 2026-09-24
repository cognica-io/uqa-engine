//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON cannot represent non-finite numbers; preserve their IEEE bits in the typed value protocol.

use super::{Serialize, Serializer, TaggedBytes};

pub(super) fn serialize<S: Serializer>(value: f64, serializer: S) -> Result<S::Ok, S::Error> {
    if value.is_finite() || !serializer.is_human_readable() {
        return serializer.serialize_f64(value);
    }
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = [b'0'; 16];
    for (index, byte) in value.to_bits().to_be_bytes().into_iter().enumerate() {
        hex[index * 2] = DIGITS[usize::from(byte >> 4)];
        hex[index * 2 + 1] = DIGITS[usize::from(byte & 15)];
    }
    TaggedBytes {
        kind: "float_bits",
        hex: std::str::from_utf8(&hex).expect("hexadecimal ASCII"),
    }
    .serialize(serializer)
}

pub(super) fn decode(hex: &str) -> Option<f64> {
    if hex.len() != 16 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let value = f64::from_bits(u64::from_str_radix(hex, 16).ok()?);
    (!value.is_finite()).then_some(value)
}

#[cfg(test)]
mod tests {
    use crate::{memory::MemoryBudget, ArrayValue, CancellationToken, JsonValueDecoder, Value};

    #[test]
    fn nonfinite_float_json_preserves_bits_and_controlled_decoding() {
        let budget = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        for bits in [
            f64::INFINITY.to_bits(),
            f64::NEG_INFINITY.to_bits(),
            f64::NAN.to_bits(),
            0xfff8_0000_0000_0001,
        ] {
            let value = Value::Float(f64::from_bits(bits));
            let encoded = serde_json::to_string(&value).unwrap();
            let Value::Float(decoded) = serde_json::from_str(&encoded).unwrap() else {
                panic!("float carrier")
            };
            assert_eq!(decoded.to_bits(), bits);
            let decoded = JsonValueDecoder::new(&budget, &token)
                .value(&encoded)
                .unwrap();
            let Value::Float(decoded_value) = *decoded else {
                panic!("controlled float carrier")
            };
            assert_eq!(decoded_value.to_bits(), bits);
            drop(decoded);
            assert_eq!(budget.used(), 0);
        }
        assert_eq!(serde_json::to_string(&Value::Float(1.5)).unwrap(), "1.5");
        assert_eq!(serde_json::to_string(&Value::Null).unwrap(), "null");
    }

    #[test]
    fn nested_nonfinite_values_keep_their_carriers_and_json_text_stays_literal() {
        let elements = vec![
            Value::Float(f64::INFINITY),
            Value::Float(f64::NEG_INFINITY),
            Value::Float(f64::from_bits(0xfff8_0000_0000_0001)),
            Value::Float(-0.0),
            Value::Null,
        ];
        let tag_text = r#"{"$uqa_type":"float_bits","hex":"7ff0000000000000"}"#;
        for value in [
            Value::List(elements.clone()),
            Value::Row(elements.clone()),
            Value::Record(vec![("floats".into(), Value::List(elements.clone()))]),
            Value::Array(ArrayValue::with_lower_bounds(elements.clone(), vec![-2]).unwrap()),
            Value::Map([("floats".into(), Value::List(elements))].into()),
            Value::Json(tag_text.into()),
            Value::JsonB(tag_text.into()),
        ] {
            let encoded = serde_json::to_string(&value).unwrap();
            let decoded: Value = serde_json::from_str(&encoded).unwrap();
            assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
            let budget = MemoryBudget::new(64 * 1024);
            let token = CancellationToken::new();
            let controlled = JsonValueDecoder::new(&budget, &token)
                .value(&encoded)
                .unwrap();
            assert_eq!(serde_json::to_string(&*controlled).unwrap(), encoded);
            drop(controlled);
            assert_eq!(budget.used(), 0);
            let exhausted = MemoryBudget::new(1);
            assert!(JsonValueDecoder::new(&exhausted, &token)
                .value(&encoded)
                .is_err());
            assert_eq!(exhausted.used(), 0);
            token.cancel();
            assert!(JsonValueDecoder::new(&budget, &token)
                .value(&encoded)
                .is_err());
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn malformed_or_finite_float_tags_remain_ordinary_maps() {
        for hex in [
            "7ff",
            "not_a_float_bits",
            "3ff0000000000000",
            "0000000000000000",
            "7ff000000000000z",
        ] {
            let source = format!(r#"{{"$uqa_type":"float_bits","hex":"{hex}"}}"#);
            assert!(matches!(
                serde_json::from_str::<Value>(&source).unwrap(),
                Value::Map(_)
            ));
        }
    }
}
