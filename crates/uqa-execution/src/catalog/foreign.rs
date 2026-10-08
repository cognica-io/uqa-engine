//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::ast::{ColumnDef, TableCheck};
use uqa_storage::{StorageBackendError, StorageBackendResult};

const FOREIGN_TABLE_SCHEMA_VERSION: u8 = 3;

pub mod reference;
pub mod wrappers;
pub use reference::ForeignServerReference;

#[derive(Debug, Clone)]
pub struct StoredForeignTable {
    pub name: String,
    /// Session-local lifetime; durable definitions always restore as permanent.
    pub persistence: uqa_sql::ast::RelationPersistence,
    pub object_id: [u8; 16],
    /// The public OIDs allocated when the foreign table was created; `None` for one created before OIDs were recorded.
    pub catalog_oids: Option<uqa_sql::catalog::relation_oids::RelationCatalogOids>,
    pub row_type_array_name: Option<String>,
    pub server_name: String,
    /// Captured server identity; absent only while converting a legacy definition at initial open.
    pub server_reference: Option<ForeignServerReference>,
    pub columns: Vec<ColumnDef>,
    pub dropped_attributes: Vec<uqa_sql::catalog::relation_attributes::DroppedAttribute>,
    pub checks: Vec<TableCheck>,
    pub options: BTreeMap<String, String>,
    pub option_order: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct PersistedForeignTableSchema {
    version: u8,
    #[serde(default)]
    object_id: [u8; 16],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catalog_oids: Option<uqa_sql::catalog::relation_oids::RelationCatalogOids>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    row_type_array_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_reference: Option<ForeignServerReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    option_order: Option<Vec<String>>,
    columns: Vec<ColumnDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dropped_attributes: Vec<uqa_sql::catalog::relation_attributes::DroppedAttribute>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    checks: Vec<TableCheck>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ForeignTableSchemaFormat {
    Current(PersistedForeignTableSchema),
    Legacy(Vec<ColumnDef>),
}

impl StoredForeignTable {
    /// Check the reader fence before any restoration step can rewrite a legacy row.
    pub(crate) fn from_catalog_row(
        row: &uqa_storage::ForeignTableRow,
        reference_format: Option<u32>,
    ) -> StorageBackendResult<(Self, bool)> {
        let (table, legacy) = Self::from_catalog(
            row.relation.qualified_name(),
            row.server_name.clone(),
            serde_json::from_str(&row.options_json)?,
            &row.columns_json,
        )?;
        reference::validate_schema_format(reference_format, legacy, &table.name)?;
        Ok((table, legacy))
    }

    pub fn from_catalog(
        name: String,
        server_name: String,
        options: BTreeMap<String, String>,
        schema_json: &str,
    ) -> StorageBackendResult<(Self, bool)> {
        let schema = serde_json::from_str::<ForeignTableSchemaFormat>(schema_json)?;
        let (schema, legacy) = match schema {
            ForeignTableSchemaFormat::Current(schema) => {
                if !matches!(schema.version, 1 | 2 | FOREIGN_TABLE_SCHEMA_VERSION) {
                    return Err(StorageBackendError::Other(format!(
                        "foreign table `{name}` has unsupported schema version {}",
                        schema.version
                    )));
                }
                reference::validate_schema_reference(
                    &name,
                    schema.version,
                    schema.server_reference,
                )?;
                if schema.catalog_oids.is_some_and(|oids| {
                    !oids.is_valid_for(
                        uqa_sql::catalog::relation_oids::RelationOidKind::ForeignTable,
                    )
                }) {
                    return Err(StorageBackendError::Other(format!(
                        "foreign table `{name}` records invalid catalog OIDs"
                    )));
                }
                let legacy = schema.version != FOREIGN_TABLE_SCHEMA_VERSION;
                (schema, legacy)
            }
            ForeignTableSchemaFormat::Legacy(columns) => (
                PersistedForeignTableSchema {
                    version: 1,
                    object_id: [0; 16],
                    catalog_oids: None,
                    row_type_array_name: None,
                    server_reference: None,
                    option_order: None,
                    columns,
                    dropped_attributes: Vec::new(),
                    checks: Vec::new(),
                },
                true,
            ),
        };
        let option_order = match schema.option_order {
            Some(order) => order,
            None if legacy => options.keys().cloned().collect(),
            None => {
                return Err(StorageBackendError::Other(format!(
                    "foreign table `{name}` has no option order"
                )))
            }
        };
        let names: BTreeSet<_> = option_order.iter().collect();
        if names.len() != option_order.len()
            || names.len() != options.len()
            || names.iter().any(|name| !options.contains_key(*name))
        {
            return Err(StorageBackendError::Other(format!(
                "foreign table `{name}` option order disagrees with stored options"
            )));
        }
        Ok((
            Self {
                name,
                persistence: uqa_sql::ast::RelationPersistence::Permanent,
                object_id: schema.object_id,
                catalog_oids: schema.catalog_oids,
                row_type_array_name: schema.row_type_array_name,
                server_name,
                server_reference: schema.server_reference,
                columns: schema.columns,
                dropped_attributes: schema.dropped_attributes,
                checks: schema.checks,
                options,
                option_order,
            },
            legacy,
        ))
    }

    /// The foreign table's public OIDs: the recorded ones, or those its identity derives.
    pub fn relation_oids(&self) -> uqa_sql::catalog::relation_oids::RelationCatalogOids {
        self.catalog_oids.unwrap_or_else(|| {
            uqa_sql::catalog::relation_oids::RelationCatalogOids::legacy(
                uqa_sql::catalog::relation_oids::RelationOidKind::ForeignTable,
                &self.object_id,
            )
        })
    }

    pub fn schema_json(&self) -> StorageBackendResult<String> {
        serde_json::to_string(&PersistedForeignTableSchema {
            version: if self.server_reference.is_some() {
                FOREIGN_TABLE_SCHEMA_VERSION
            } else {
                1
            },
            object_id: self.object_id,
            catalog_oids: self.catalog_oids,
            row_type_array_name: self.row_type_array_name.clone(),
            server_reference: self.server_reference,
            option_order: Some(self.option_order.clone()),
            columns: self.columns.clone(),
            dropped_attributes: self.dropped_attributes.clone(),
            checks: self.checks.clone(),
        })
        .map_err(StorageBackendError::from)
    }

    pub fn fdw_definition(&self) -> uqa_fdw::ForeignTable {
        uqa_fdw::ForeignTable {
            name: self.name.clone(),
            server_name: self.server_name.clone(),
            columns: self
                .columns
                .iter()
                .map(|column| uqa_fdw::ColumnDef {
                    name: column.name.clone(),
                    ty: sql_column_type_to_fdw(&column.ty),
                })
                .collect(),
            options: self.options.clone(),
        }
    }

    pub fn persist(
        &self,
        catalog: Option<&dyn uqa_storage::CatalogFacade>,
        relation: &RelationIdentity,
        security: &super::security::BoundTableSecurity,
    ) -> StorageBackendResult<()> {
        if self.persistence != uqa_sql::ast::RelationPersistence::Temporary {
            if let Some(catalog) = catalog {
                catalog.save_foreign_table(&self.catalog_row(relation, security)?)?;
            }
        }
        Ok(())
    }

    pub fn catalog_row(
        &self,
        relation: &RelationIdentity,
        security: &super::security::BoundTableSecurity,
    ) -> StorageBackendResult<uqa_storage::ForeignTableRow> {
        Ok(uqa_storage::ForeignTableRow {
            relation: relation.clone(),
            security: security.row().into(),
            server_name: self.server_name.clone(),
            columns_json: self.schema_json()?,
            options_json: serde_json::to_string(&self.options)?,
        })
    }
}

pub fn sql_column_type_to_fdw(column_type: &uqa_sql::ast::ColumnType) -> uqa_fdw::ColumnType {
    match column_type {
        uqa_sql::ast::ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached foreign-table projection")
        }
        uqa_sql::ast::ColumnType::Boolean => uqa_fdw::ColumnType::Bool,
        uqa_sql::ast::ColumnType::Void => uqa_fdw::ColumnType::Void,
        uqa_sql::ast::ColumnType::SmallInteger => uqa_fdw::ColumnType::SmallInteger,
        uqa_sql::ast::ColumnType::Integer => uqa_fdw::ColumnType::Integer,
        uqa_sql::ast::ColumnType::BigInteger => uqa_fdw::ColumnType::BigInteger,
        uqa_sql::ast::ColumnType::Oid => uqa_fdw::ColumnType::Oid,
        uqa_sql::ast::ColumnType::Xid => uqa_fdw::ColumnType::Xid,
        uqa_sql::ast::ColumnType::Real => uqa_fdw::ColumnType::Real,
        uqa_sql::ast::ColumnType::DoublePrecision => uqa_fdw::ColumnType::DoublePrecision,
        uqa_sql::ast::ColumnType::Numeric { precision, scale } => uqa_fdw::ColumnType::Numeric {
            precision: *precision,
            scale: *scale,
        },
        uqa_sql::ast::ColumnType::Text => uqa_fdw::ColumnType::Text,
        uqa_sql::ast::ColumnType::RefCursor => uqa_fdw::ColumnType::RefCursor,
        uqa_sql::ast::ColumnType::Name => uqa_fdw::ColumnType::Name,
        uqa_sql::ast::ColumnType::Uuid => uqa_fdw::ColumnType::Uuid,
        uqa_sql::ast::ColumnType::Varchar(length) => uqa_fdw::ColumnType::Varchar(*length),
        uqa_sql::ast::ColumnType::Bpchar => uqa_fdw::ColumnType::Bpchar,
        uqa_sql::ast::ColumnType::Character(length) => uqa_fdw::ColumnType::Character(*length),
        uqa_sql::ast::ColumnType::Json => uqa_fdw::ColumnType::Json,
        uqa_sql::ast::ColumnType::JsonB => uqa_fdw::ColumnType::JsonB,
        uqa_sql::ast::ColumnType::Date => uqa_fdw::ColumnType::Date,
        uqa_sql::ast::ColumnType::Time | uqa_sql::ast::ColumnType::TimePrecision(_) => {
            uqa_fdw::ColumnType::Time
        }
        uqa_sql::ast::ColumnType::TimeTz | uqa_sql::ast::ColumnType::TimeTzPrecision(_) => {
            uqa_fdw::ColumnType::TimeTz
        }
        uqa_sql::ast::ColumnType::Timestamp | uqa_sql::ast::ColumnType::TimestampPrecision(_) => {
            uqa_fdw::ColumnType::Timestamp
        }
        uqa_sql::ast::ColumnType::TimestampTz
        | uqa_sql::ast::ColumnType::TimestampTzPrecision(_) => uqa_fdw::ColumnType::TimestampTz,
        uqa_sql::ast::ColumnType::Interval
        | uqa_sql::ast::ColumnType::IntervalWithFields { .. } => uqa_fdw::ColumnType::Interval,
        uqa_sql::ast::ColumnType::Range(subtype) => {
            uqa_fdw::ColumnType::Range(sql_range_subtype_to_fdw(*subtype))
        }
        uqa_sql::ast::ColumnType::Multirange(subtype) => {
            uqa_fdw::ColumnType::Multirange(sql_range_subtype_to_fdw(*subtype))
        }
        uqa_sql::ast::ColumnType::Bytea => uqa_fdw::ColumnType::Bytes,
        uqa_sql::ast::ColumnType::InternalChar => uqa_fdw::ColumnType::InternalChar,
        uqa_sql::ast::ColumnType::Regproc => uqa_fdw::ColumnType::Regproc,
        uqa_sql::ast::ColumnType::Regprocedure => uqa_fdw::ColumnType::Regprocedure,
        uqa_sql::ast::ColumnType::Regclass => uqa_fdw::ColumnType::Regclass,
        uqa_sql::ast::ColumnType::Regcollation => uqa_fdw::ColumnType::Regcollation,
        uqa_sql::ast::ColumnType::Regnamespace => uqa_fdw::ColumnType::Regnamespace,
        uqa_sql::ast::ColumnType::Regrole => uqa_fdw::ColumnType::Regrole,
        uqa_sql::ast::ColumnType::Regtype => uqa_fdw::ColumnType::Regtype,
        uqa_sql::ast::ColumnType::PgNodeTree => uqa_fdw::ColumnType::PgNodeTree,
        uqa_sql::ast::ColumnType::AclItem => uqa_fdw::ColumnType::AclItem,
        uqa_sql::ast::ColumnType::Int2Vector => uqa_fdw::ColumnType::Int2Vector,
        uqa_sql::ast::ColumnType::OidVector => uqa_fdw::ColumnType::OidVector,
        uqa_sql::ast::ColumnType::AnyArray => uqa_fdw::ColumnType::AnyArray,
        uqa_sql::ast::ColumnType::Record => uqa_fdw::ColumnType::Record,
        uqa_sql::ast::ColumnType::Vector(dimension) => uqa_fdw::ColumnType::Vector(*dimension),
        uqa_sql::ast::ColumnType::Tensor(dimension) => uqa_fdw::ColumnType::Tensor(*dimension),
        uqa_sql::ast::ColumnType::Array(element) => {
            uqa_fdw::ColumnType::Array(Box::new(sql_column_type_to_fdw(element)))
        }
        uqa_sql::ast::ColumnType::Enum(reference) => uqa_fdw::ColumnType::Enum {
            schema: reference.schema.clone(),
            name: reference.name.clone(),
            oid: reference.oid,
        },
        uqa_sql::ast::ColumnType::Composite(reference) => uqa_fdw::ColumnType::Composite {
            schema: reference.schema.clone(),
            name: reference.name.clone(),
            oid: reference.oid,
        },
        uqa_sql::ast::ColumnType::Domain {
            schema,
            name,
            oid,
            base,
            ..
        } => uqa_fdw::ColumnType::Domain {
            schema: schema.clone(),
            name: name.clone(),
            oid: *oid,
            base: Box::new(sql_column_type_to_fdw(base)),
        },
    }
}

fn sql_range_subtype_to_fdw(subtype: uqa_sql::ast::RangeSubtype) -> uqa_fdw::RangeSubtype {
    match subtype {
        uqa_sql::ast::RangeSubtype::Integer => uqa_fdw::RangeSubtype::Integer,
        uqa_sql::ast::RangeSubtype::BigInteger => uqa_fdw::RangeSubtype::BigInteger,
        uqa_sql::ast::RangeSubtype::Numeric => uqa_fdw::RangeSubtype::Numeric,
        uqa_sql::ast::RangeSubtype::Date => uqa_fdw::RangeSubtype::Date,
        uqa_sql::ast::RangeSubtype::Timestamp => uqa_fdw::RangeSubtype::Timestamp,
        uqa_sql::ast::RangeSubtype::TimestampTz => uqa_fdw::RangeSubtype::TimestampTz,
    }
}

pub mod lookup;
pub mod reads;

pub mod restoration;

pub mod servers;
