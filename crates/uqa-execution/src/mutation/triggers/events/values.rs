//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Event payloads use the physical codec, preserving opaque and nonfinite Value representations.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use uqa_core::Value;

use crate::spill::{decode_physical_row_record, encode_physical_row_record};
use crate::PhysicalRow;

pub(crate) fn serialize<S: Serializer>(value: &Value, serializer: S) -> Result<S::Ok, S::Error> {
    let encoded = encode_physical_row_record(&PhysicalRow::from_values(vec![value.clone()]), 1)
        .map_err(serde::ser::Error::custom)?;
    serializer.serialize_bytes(&encoded)
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Value, D::Error> {
    struct EncodedValue;
    impl de::Visitor<'_> for EncodedValue {
        type Value = Value;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a physical trigger value")
        }

        fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Value, E> {
            let row = decode_physical_row_record(value, 1).map_err(E::custom)?;
            let [value] = <[Value; 1]>::try_from(row.into_physical_values())
                .map_err(|_| E::custom("invalid trigger value width"))?;
            Ok(value)
        }
    }
    deserializer.deserialize_byte_buf(EncodedValue)
}

#[derive(Serialize, Deserialize)]
struct PhysicalValue(#[serde(with = "self")] Value);

pub(crate) mod list {
    use super::{de, Deserializer, Serializer, Value};

    pub(crate) fn serialize<S: Serializer>(
        values: &[Value],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        super::serialize(&Value::List(values.to_vec()), serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Value>, D::Error> {
        match super::deserialize(deserializer)? {
            Value::List(values) => Ok(values),
            _ => Err(de::Error::custom("invalid trigger key")),
        }
    }
}

pub(crate) mod optional_list {
    use super::{de, Deserialize, Deserializer, PhysicalValue, Serialize, Serializer, Value};

    #[expect(
        clippy::ref_option,
        reason = "serde field serializers borrow the complete field"
    )]
    pub(crate) fn serialize<S: Serializer>(
        values: &Option<Vec<Value>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        values
            .as_ref()
            .map(|values| PhysicalValue(Value::List(values.clone())))
            .serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<Value>>, D::Error> {
        Option::<PhysicalValue>::deserialize(deserializer)?
            .map(|PhysicalValue(value)| match value {
                Value::List(values) => Ok(values),
                _ => Err(de::Error::custom("invalid replacement trigger key")),
            })
            .transpose()
    }
}
