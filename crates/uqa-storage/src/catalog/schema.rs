//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uqa_core::catalog_schema::SchemaRow as LegacySchemaRow;
pub use uqa_core::catalog_schema::{
    BoundSchemaRow, SchemaAclEntry, SchemaPrivileges, SchemaTupleIdentity,
};

/// A catalog read distinguishes old names from already captured role identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaRow {
    Bound(BoundSchemaRow),
    Legacy(LegacySchemaRow),
}

impl SchemaRow {
    pub fn name(&self) -> &str {
        match self {
            Self::Bound(row) => &row.name,
            Self::Legacy(row) => &row.name,
        }
    }

    pub fn legacy(name: impl Into<String>) -> Self {
        Self::Legacy(LegacySchemaRow::legacy(name))
    }

    pub fn bootstrap(name: impl Into<String>) -> Self {
        Self::Bound(BoundSchemaRow::bootstrap(name))
    }
}

impl From<BoundSchemaRow> for SchemaRow {
    fn from(row: BoundSchemaRow) -> Self {
        Self::Bound(row)
    }
}

impl Serialize for SchemaRow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Bound<'a> {
            schema_security_format: u32,
            #[serde(flatten)]
            row: &'a BoundSchemaRow,
        }
        match self {
            Self::Legacy(row) => row.serialize(serializer),
            Self::Bound(row) => {
                if row.tuple.is_some_and(|tuple| !tuple.is_valid()) {
                    return Err(serde::ser::Error::custom(
                        "invalid schema catalog tuple identity",
                    ));
                }
                Bound {
                    schema_security_format: if row.tuple.is_some() { 2 } else { 1 },
                    row,
                }
                .serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for SchemaRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(version) = value.get("schema_security_format") {
            let version = version.as_u64();
            if !matches!(version, Some(1 | 2)) {
                return Err(D::Error::custom("unsupported schema security format"));
            }
            if version == Some(1) && value.get("tuple").is_some() {
                return Err(D::Error::custom(
                    "legacy schema security contains a tuple identity",
                ));
            }
            let row: BoundSchemaRow = serde_json::from_value(value).map_err(D::Error::custom)?;
            if version == Some(2) && !row.tuple.is_some_and(SchemaTupleIdentity::is_valid) {
                return Err(D::Error::custom("invalid schema catalog tuple identity"));
            }
            Ok(Self::Bound(row))
        } else {
            if value.get("tuple").is_some() {
                return Err(D::Error::custom(
                    "schema tuple identity has no format marker",
                ));
            }
            serde_json::from_value(value)
                .map(Self::Legacy)
                .map_err(D::Error::custom)
        }
    }
}

#[cfg(test)]
mod tests;
