//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_fdw::ColumnType as ForeignColumnType;
use uqa_sql::ast::{ColumnDef, ColumnType, TableCheck};
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
    /// Captured server identity. The declaration's relation and row type are visible before its server is bound; legacy definitions also acquire this identity during initial restoration.
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
                        &schema.object_id,
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
                if self.server_reference.is_some() {
                    super::tuples::publication::queue_relation(
                        catalog,
                        relation,
                        self.object_id,
                        true,
                    )?;
                }
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

pub fn sql_column_type_to_fdw(column_type: &ColumnType) -> ForeignColumnType {
    match column_type {
        ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached foreign-table projection")
        }
        ColumnType::Boolean => ForeignColumnType::Bool,
        ColumnType::Void => ForeignColumnType::Void,
        ColumnType::SmallInteger => ForeignColumnType::SmallInteger,
        ColumnType::Integer => ForeignColumnType::Integer,
        ColumnType::BigInteger => ForeignColumnType::BigInteger,
        ColumnType::Oid => ForeignColumnType::Oid,
        ColumnType::Xid => ForeignColumnType::Xid,
        ColumnType::Real => ForeignColumnType::Real,
        ColumnType::DoublePrecision => ForeignColumnType::DoublePrecision,
        ColumnType::Numeric { precision, scale } => ForeignColumnType::Numeric {
            precision: *precision,
            scale: *scale,
        },
        ColumnType::Text => ForeignColumnType::Text,
        ColumnType::RefCursor => ForeignColumnType::RefCursor,
        ColumnType::Xml => ForeignColumnType::Xml,
        ColumnType::Name => ForeignColumnType::Name,
        ColumnType::Uuid => ForeignColumnType::Uuid,
        ColumnType::Varchar(length) => ForeignColumnType::Varchar(*length),
        ColumnType::Bpchar => ForeignColumnType::Bpchar,
        ColumnType::Character(length) => ForeignColumnType::Character(*length),
        ColumnType::Json => ForeignColumnType::Json,
        ColumnType::JsonB => ForeignColumnType::JsonB,
        ColumnType::Date => ForeignColumnType::Date,
        ColumnType::Time | ColumnType::TimePrecision(_) => ForeignColumnType::Time,
        ColumnType::TimeTz | ColumnType::TimeTzPrecision(_) => ForeignColumnType::TimeTz,
        ColumnType::Timestamp | ColumnType::TimestampPrecision(_) => ForeignColumnType::Timestamp,
        ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_) => {
            ForeignColumnType::TimestampTz
        }
        ColumnType::Interval | ColumnType::IntervalWithFields { .. } => ForeignColumnType::Interval,
        ColumnType::Range(subtype) => ForeignColumnType::Range(sql_range_subtype_to_fdw(*subtype)),
        ColumnType::Multirange(subtype) => {
            ForeignColumnType::Multirange(sql_range_subtype_to_fdw(*subtype))
        }
        ColumnType::DeclaredRange(reference) => ForeignColumnType::DeclaredRange {
            schema: reference.schema.clone(),
            name: reference.name.clone(),
            oid: reference.oid,
        },
        ColumnType::DeclaredMultirange(reference) => ForeignColumnType::DeclaredMultirange {
            schema: reference.schema.clone(),
            name: reference.name.clone(),
            oid: reference.oid,
        },
        ColumnType::Bytea => ForeignColumnType::Bytes,
        ColumnType::Bit(length) => ForeignColumnType::Bit(*length),
        ColumnType::VarBit(length) => ForeignColumnType::VarBit(*length),
        ColumnType::InternalChar => ForeignColumnType::InternalChar,
        ColumnType::Regproc => ForeignColumnType::Regproc,
        ColumnType::Regprocedure => ForeignColumnType::Regprocedure,
        ColumnType::Regclass => ForeignColumnType::Regclass,
        ColumnType::Regcollation => ForeignColumnType::Regcollation,
        ColumnType::Regnamespace => ForeignColumnType::Regnamespace,
        ColumnType::Regrole => ForeignColumnType::Regrole,
        ColumnType::Regtype => ForeignColumnType::Regtype,
        ColumnType::PgNodeTree => ForeignColumnType::PgNodeTree,
        ColumnType::AclItem => ForeignColumnType::AclItem,
        ColumnType::Int2Vector => ForeignColumnType::Int2Vector,
        ColumnType::OidVector => ForeignColumnType::OidVector,
        ColumnType::AnyArray => ForeignColumnType::AnyArray,
        ColumnType::Record => ForeignColumnType::Record,
        ColumnType::Vector(dimension) => ForeignColumnType::Vector(*dimension),
        ColumnType::Tensor(dimension) => ForeignColumnType::Tensor(*dimension),
        ColumnType::Array(element) => {
            ForeignColumnType::Array(Box::new(sql_column_type_to_fdw(element)))
        }
        ColumnType::Enum(reference) => ForeignColumnType::Enum {
            schema: reference.schema.clone(),
            name: reference.name.clone(),
            oid: reference.oid,
        },
        ColumnType::Composite(reference) => ForeignColumnType::Composite {
            schema: reference.schema.clone(),
            name: reference.name.clone(),
            oid: reference.oid,
        },
        ColumnType::Domain {
            schema,
            name,
            oid,
            base,
            ..
        } => ForeignColumnType::Domain {
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
