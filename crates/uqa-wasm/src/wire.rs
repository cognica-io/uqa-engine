//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serialize borrowed engine results into the browser's JSON protocol.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde::ser::{Error as _, SerializeMap, SerializeStruct};
use serde::{Serialize, Serializer};
use serde_json::Value as JSON;
use uqa_core::Value;
use uqa_engine::SQLResult;

use super::{finite_number, safe_i64, safe_u64, BASE64};

pub(super) mod retrieval;

pub(super) fn success<T: Serialize>(value: T) -> Result<String, String> {
    #[derive(Serialize)]
    struct Success<T> {
        ok: T,
    }
    serde_json::to_string(&Success { ok: value }).map_err(|error| error.to_string())
}

pub(super) struct WireValue<'a>(pub(super) &'a Value);

impl Serialize for WireValue<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Datum(datum) => {
                let value = uqa_sql::expr::datums::read(datum).map_err(S::Error::custom)?;
                WireValue(&value).serialize(serializer)
            }
            Value::Null => serializer.serialize_none(),
            Value::Void => serializer.serialize_str(""),
            Value::Bool(value) => serializer.serialize_bool(*value),
            Value::Int(value) => safe_i64(*value, "SQL integer")
                .map_err(S::Error::custom)?
                .serialize(serializer),
            Value::Float(value) => finite_number(*value, "SQL float")
                .map_err(S::Error::custom)?
                .serialize(serializer),
            Value::Decimal(value) => serializer.serialize_str(&value.to_sql_string()),
            Value::Str(value) | Value::FixedChar(value) => serializer.serialize_str(value),
            Value::Json(text) | Value::JsonB(text) => {
                let value = serde_json::from_str::<JSON>(text).map_err(|err| {
                    S::Error::custom(format_args!("invalid SQL JSON value: {err}"))
                })?;
                value.serialize(serializer)
            }
            Value::Bytes(value) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("$bytes", &BASE64.encode(value))?;
                map.end()
            }
            value @ Value::BitString(_) => serializer
                .serialize_str(&uqa_sql::expr::value_to_string(value).map_err(S::Error::custom)?),
            Value::Temporal(value) => serializer.serialize_str(&value.to_sql_string()),
            Value::Enum(value) => Err(S::Error::custom(uqa_sql::expr::catalog_output_required(
                value,
            ))),
            Value::LegacyVector(vector) => WireValues(vector.elements()).serialize(serializer),
            Value::Array(array) => WireValues(array.elements()).serialize(serializer),
            Value::List(values) => WireValues(values).serialize(serializer),
            Value::Row(row) => WireValues(row.values()).serialize(serializer),
            Value::Record(record) => {
                let names = record.descriptor().field_names();
                if names.windows(2).all(|pair| pair[0] < pair[1]) {
                    return serializer
                        .collect_map(record.iter().map(|(name, value)| (name, WireValue(value))));
                }
                // Convert fields in declaration order, including shadowed values, then emit the last value for each name in key order.
                let mut fields = BTreeMap::new();
                for (name, value) in record {
                    fields.insert(
                        name,
                        serde_json::value::to_raw_value(&WireValue(value))
                            .map_err(S::Error::custom)?,
                    );
                }
                fields.serialize(serializer)
            }
            Value::Map(map) => WireRow(map).serialize(serializer),
        }
    }
}

pub(super) struct WireValues<'a>(pub(super) &'a [Value]);

impl Serialize for WireValues<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(WireValue))
    }
}

pub(super) struct WireRow<'a>(pub(super) &'a BTreeMap<String, Value>);

impl Serialize for WireRow<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(name, value)| (name, WireValue(value))))
    }
}

struct WireRows<'a>(&'a [BTreeMap<String, Value>]);

impl Serialize for WireRows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(WireRow))
    }
}

pub(super) struct WireResult<'a> {
    pub(super) columns: &'a [String],
    pub(super) rows: &'a [BTreeMap<String, Value>],
    pub(super) affected_rows: u64,
}

impl<'a> From<&'a SQLResult> for WireResult<'a> {
    fn from(result: &'a SQLResult) -> Self {
        Self {
            columns: &result.columns,
            rows: &result.rows,
            affected_rows: result.affected_rows,
        }
    }
}

impl Serialize for WireResult<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut result = serializer.serialize_struct("SQLResult", 3)?;
        result.serialize_field("affectedRows", &self.affected_rows)?;
        result.serialize_field("columns", self.columns)?;
        result.serialize_field("rows", &WireRows(self.rows))?;
        // Row conversion errors precede an out-of-range affected count.
        safe_u64(self.affected_rows, "affected row count").map_err(S::Error::custom)?;
        result.end()
    }
}

pub(super) struct WireBatch<'a>(pub(super) &'a [SQLResult]);

impl Serialize for WireBatch<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(WireResult::from))
    }
}

pub(super) fn notices(notices: Vec<uqa_sql::SQLNotice>) -> Result<String, String> {
    #[derive(Serialize)]
    struct Notice {
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        level: &'static str,
        message: String,
        sqlstate: String,
    }
    let notices: Vec<_> = notices
        .into_iter()
        .map(|notice| Notice {
            detail: notice.detail,
            hint: notice.hint,
            level: notice.level.as_str(),
            message: notice.message,
            sqlstate: notice.sqlstate,
        })
        .collect();
    success(notices)
}

#[cfg(test)]
mod tests;
