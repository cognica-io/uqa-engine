//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently addressable JSON rows used only by `IndexedSpill`.

use std::collections::BTreeMap;

use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use uqa_core::{DecimalValue, TemporalValue, Value};
use uqa_sql::ResultRow;

use crate::physical::ExecResult;

use super::spill_error;

pub(crate) struct ExactRow<'a>(pub(crate) &'a ResultRow);

impl Serialize for ExactRow<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            map.serialize_entry(name, &ExactValue(value))?;
        }
        map.end()
    }
}

struct ExactValue<'a>(&'a Value);

impl Serialize for ExactValue<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0 {
            Value::Null => serializer.serialize_unit_variant("SpillValue", 0, "Null"),
            Value::Bool(value) => {
                serializer.serialize_newtype_variant("SpillValue", 1, "Bool", value)
            }
            Value::Int(value) => {
                serializer.serialize_newtype_variant("SpillValue", 2, "Int", value)
            }
            Value::Float(value) => {
                serializer.serialize_newtype_variant("SpillValue", 3, "Float", &value.to_bits())
            }
            Value::Str(value) => {
                serializer.serialize_newtype_variant("SpillValue", 4, "Str", value)
            }
            Value::FixedChar(value) => {
                serializer.serialize_newtype_variant("SpillValue", 10, "FixedChar", value)
            }
            Value::Bytes(value) => {
                serializer.serialize_newtype_variant("SpillValue", 5, "Bytes", value)
            }
            Value::Temporal(value) => {
                serializer.serialize_newtype_variant("SpillValue", 6, "Temporal", value)
            }
            Value::Decimal(value) => serializer.serialize_newtype_variant(
                "SpillValue",
                7,
                "Decimal",
                &value.to_sql_string(),
            ),
            Value::List(value) => {
                serializer.serialize_newtype_variant("SpillValue", 8, "List", &ExactValues(value))
            }
            Value::Map(value) => {
                serializer.serialize_newtype_variant("SpillValue", 9, "Map", &ExactMap(value))
            }
        }
    }
}

struct ExactValues<'a>(&'a [Value]);

impl Serialize for ExactValues<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            sequence.serialize_element(&ExactValue(value))?;
        }
        sequence.end()
    }
}

struct ExactMap<'a>(&'a BTreeMap<String, Value>);

impl Serialize for ExactMap<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, &ExactValue(value))?;
        }
        map.end()
    }
}

pub(crate) fn decode_row(record: &[u8]) -> ExecResult<ResultRow> {
    let stored: BTreeMap<String, StoredValue> =
        serde_json::from_slice(record).map_err(|error| {
            spill_error(format!("failed to deserialize indexed spill row: {error}"))
        })?;
    stored
        .into_iter()
        .map(|(name, value)| value.into_value().map(|value| (name, value)))
        .collect()
}

#[derive(Deserialize)]
enum StoredValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(String),
    FixedChar(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(String),
    List(Vec<StoredValue>),
    Map(BTreeMap<String, StoredValue>),
}

impl StoredValue {
    fn into_value(self) -> ExecResult<Value> {
        match self {
            Self::Null => Ok(Value::Null),
            Self::Bool(value) => Ok(Value::Bool(value)),
            Self::Int(value) => Ok(Value::Int(value)),
            Self::Float(bits) => Ok(Value::Float(f64::from_bits(bits))),
            Self::Str(value) => Ok(Value::Str(value)),
            Self::FixedChar(value) => Ok(Value::FixedChar(value)),
            Self::Bytes(value) => Ok(Value::Bytes(value)),
            Self::Temporal(value) => Ok(Value::Temporal(value)),
            Self::Decimal(value) => DecimalValue::parse(&value)
                .map(Value::Decimal)
                .ok_or_else(|| spill_error(format!("invalid decimal in spill file: {value}"))),
            Self::List(values) => values
                .into_iter()
                .map(Self::into_value)
                .collect::<ExecResult<Vec<_>>>()
                .map(Value::List),
            Self::Map(values) => values
                .into_iter()
                .map(|(key, value)| value.into_value().map(|value| (key, value)))
                .collect::<ExecResult<BTreeMap<_, _>>>()
                .map(Value::Map),
        }
    }
}
