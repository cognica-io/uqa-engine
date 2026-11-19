//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public scalar visitors preserve buffered tagged-field diagnostics without buffering a tree.

use crate::{work::Work, AnalysisError, AnalysisResult};
use serde::de::{Error as _, Visitor};
use serde_json::Value;

pub(super) struct Scalar<'a>(pub &'a Value);

/// Observe the public deserializer's optional map representation of a scalar number.
pub(super) fn number_map(
    value: &Value,
) -> Result<Option<serde_json::Map<String, Value>>, serde_json::Error> {
    struct Map;
    impl<'de> Visitor<'de> for Map {
        type Value = Option<serde_json::Map<String, Value>>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("any value")
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut result = serde_json::Map::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                result.insert(key, Value::String(value));
            }
            Ok(Some(result))
        }
    }
    serde::Deserializer::deserialize_any(value, Map)
}

struct BufferedNumber;
impl<'de> serde::de::Visitor<'de> for BufferedNumber {
    type Value = ();
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("any value")
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(())
    }
}

/// Tagged values collect their complete body before visiting typed fields. Preserve the public visitor's numeric admission and error order without copying that body.
pub(super) async fn admit(
    value: &Value,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<()> {
    enum Frame<'a> {
        Value(&'a Value),
        Array(std::slice::Iter<'a, Value>),
        Object(serde_json::map::Iter<'a>),
    }
    let mut stack = vec![Frame::Value(value)];
    while let Some(frame) = stack.pop() {
        work.checkpoint().await?;
        match frame {
            Frame::Value(value @ Value::Number(_)) => {
                serde::Deserializer::deserialize_any(value, BufferedNumber)?;
            }
            Frame::Value(Value::Array(values)) => stack.push(Frame::Array(values.iter())),
            Frame::Value(Value::Object(values)) => stack.push(Frame::Object(values.iter())),
            Frame::Value(_) => (),
            Frame::Array(mut values) => {
                if let Some(value) = values.next() {
                    stack.push(Frame::Array(values));
                    stack.push(Frame::Value(value));
                }
            }
            Frame::Object(mut values) => {
                if let Some((_, value)) = values.next() {
                    stack.push(Frame::Object(values));
                    stack.push(Frame::Value(value));
                }
            }
        }
    }
    Ok(())
}

pub(super) struct Reject<'a>(pub &'a str, pub &'static str);
impl<'de> serde::de::Visitor<'de> for Reject<'_> {
    type Value = ();
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Err(A::Error::missing_field(self.1))
    }
}

struct Enumeration<V> {
    name: &'static str,
    variants: &'static [&'static str],
    visitor: V,
}
impl<'de, V: Visitor<'de>> Visitor<'de> for Enumeration<V> {
    type Value = V::Value;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("string or map")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<V::Value, A::Error> {
        serde::Deserializer::deserialize_enum(
            serde::de::value::MapAccessDeserializer::new(map),
            self.name,
            self.variants,
            self.visitor,
        )
    }
}

impl<'de> serde::Deserializer<'de> for Scalar<'de> {
    type Error = serde_json::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        // Tagged fields have already selected their native numeric representation.
        if let Value::Number(number) = self.0 {
            if let Some(value) = number.as_u64() {
                return visitor.visit_u64(value);
            }
            if let Some(value) = number.as_i64() {
                return visitor.visit_i64(value);
            }
            if let Some(value) = number.as_f64() {
                return visitor.visit_f64(value);
            }
        }
        self.0.deserialize_any(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        match self.0 {
            Value::String(_) | Value::Object(_) => self.0.deserialize_enum(name, variants, visitor),
            Value::Number(_) => self.deserialize_any(Enumeration {
                name,
                variants,
                visitor,
            }),
            value => Err(serde_json::Error::invalid_type(
                super::unexpected(value, true),
                &"string or map",
            )),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes byte_buf
        option unit unit_struct newtype_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}
