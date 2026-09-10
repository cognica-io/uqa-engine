//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::ast::{ColumnDef, TableCheck};
use uqa_storage::{StorageBackendError, StorageBackendResult};

const FOREIGN_TABLE_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone)]
pub struct StoredForeignTable {
    pub name: String,
    pub object_id: [u8; 16],
    pub server_name: String,
    pub columns: Vec<ColumnDef>,
    pub checks: Vec<TableCheck>,
    pub options: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct PersistedForeignTableSchema {
    version: u8,
    #[serde(default)]
    object_id: [u8; 16],
    columns: Vec<ColumnDef>,
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
    pub fn from_catalog(
        name: String,
        server_name: String,
        options: BTreeMap<String, String>,
        schema_json: &str,
    ) -> StorageBackendResult<(Self, bool)> {
        let schema = serde_json::from_str::<ForeignTableSchemaFormat>(schema_json)?;
        let (object_id, columns, checks, legacy) = match schema {
            ForeignTableSchemaFormat::Current(schema) => {
                if schema.version != FOREIGN_TABLE_SCHEMA_VERSION {
                    return Err(StorageBackendError::Other(format!(
                        "foreign table `{name}` has unsupported schema version {}",
                        schema.version
                    )));
                }
                (schema.object_id, schema.columns, schema.checks, false)
            }
            ForeignTableSchemaFormat::Legacy(columns) => ([0; 16], columns, Vec::new(), true),
        };
        Ok((
            Self {
                name,
                object_id,
                server_name,
                columns,
                checks,
                options,
            },
            legacy,
        ))
    }

    pub fn schema_json(&self) -> StorageBackendResult<String> {
        serde_json::to_string(&PersistedForeignTableSchema {
            version: FOREIGN_TABLE_SCHEMA_VERSION,
            object_id: self.object_id,
            columns: self.columns.clone(),
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

    pub fn catalog_row(
        &self,
        relation: &RelationIdentity,
        security: &super::security::TableSecurity,
    ) -> StorageBackendResult<uqa_storage::ForeignTableRow> {
        Ok(uqa_storage::ForeignTableRow {
            relation: relation.clone(),
            role_owner: security.role_owner.clone(),
            acl: security.acl.clone(),
            column_acls: security.column_acls.clone(),
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
        uqa_sql::ast::ColumnType::Domain {
            schema,
            name,
            oid,
            base,
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
