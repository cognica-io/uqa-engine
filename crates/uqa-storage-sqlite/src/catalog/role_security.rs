//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` storage classes distinguish legacy owner names from encoded role identities.

use super::{Result, SQLiteError, SchemaRow};
use rusqlite::types::{Value, ValueRef};
use uqa_core::{
    catalog_acl::{BoundRelationSecurity, LegacyRelationSecurity},
    catalog_role::RoleIdentity,
    catalog_schema::{BoundSchemaRow, SchemaRow as LegacySchemaRow},
};
use uqa_storage::RelationSecurityRow;

fn encode_identity(identity: RoleIdentity) -> Result<Value> {
    if !identity.is_valid() {
        return Err(SQLiteError::StorageBackend(
            "invalid catalog role identity".into(),
        ));
    }
    let oid = u32::try_from(identity.oid).expect("validated role OID fits in u32");
    let mut bytes = Vec::with_capacity(21);
    bytes.push(1);
    bytes.extend_from_slice(&oid.to_be_bytes());
    bytes.extend_from_slice(&identity.object_id);
    Ok(Value::Blob(bytes))
}

fn decode_identity(bytes: &[u8]) -> Result<RoleIdentity> {
    if bytes.len() != 21 || bytes[0] != 1 {
        return Err(SQLiteError::StorageBackend(
            "invalid catalog role identity encoding".into(),
        ));
    }
    let identity = RoleIdentity {
        oid: i64::from(u32::from_be_bytes(
            bytes[1..5].try_into().expect("OID width checked"),
        )),
        object_id: bytes[5..].try_into().expect("incarnation width checked"),
    };
    if !identity.is_valid() {
        return Err(SQLiteError::StorageBackend(
            "invalid catalog role identity".into(),
        ));
    }
    Ok(identity)
}

pub(super) fn encode_sequence(
    row: &uqa_storage::SequenceSecurityRow,
) -> Result<(Value, Option<String>)> {
    use uqa_storage::SequenceSecurityRow;
    match row {
        SequenceSecurityRow::Bound(row) => Ok((
            encode_identity(row.role_owner)?,
            row.acl.as_ref().map(serde_json::to_string).transpose()?,
        )),
        SequenceSecurityRow::Legacy(row) => Ok((
            Value::Text(row.role_owner.clone()),
            row.acl.as_ref().map(serde_json::to_string).transpose()?,
        )),
    }
}

pub(super) fn decode_sequence(
    owner: ValueRef<'_>,
    acl: Option<&str>,
) -> Result<uqa_storage::SequenceSecurityRow> {
    use uqa_core::catalog_sequence::{BoundSequenceSecurity, LegacySequenceSecurity};
    use uqa_storage::SequenceSecurityRow;
    match owner {
        ValueRef::Text(_) => Ok(SequenceSecurityRow::Legacy(LegacySequenceSecurity {
            role_owner: super::native::string(owner)?,
            acl: acl.map(serde_json::from_str).transpose()?,
        })),
        ValueRef::Blob(bytes) => Ok(SequenceSecurityRow::Bound(BoundSequenceSecurity {
            role_owner: decode_identity(bytes)?,
            acl: acl.map(serde_json::from_str).transpose()?,
        })),
        _ => Err(SQLiteError::StorageBackend(
            "sequence owner has an invalid storage class".into(),
        )),
    }
}

pub(super) fn encode_schema(row: &SchemaRow) -> Result<(Value, Option<String>)> {
    match row {
        SchemaRow::Bound(row) => Ok((
            encode_identity(row.role_owner)?,
            row.acl.as_ref().map(serde_json::to_string).transpose()?,
        )),
        SchemaRow::Legacy(row) => Ok((
            Value::Text(row.role_owner.clone()),
            row.acl.as_ref().map(serde_json::to_string).transpose()?,
        )),
    }
}

pub(super) fn decode_schema(
    name: String,
    owner: ValueRef<'_>,
    acl: Option<&str>,
) -> Result<SchemaRow> {
    match owner {
        ValueRef::Text(_) => Ok(SchemaRow::Legacy(LegacySchemaRow {
            name,
            role_owner: super::native::string(owner)?,
            acl: acl.map(serde_json::from_str).transpose()?,
        })),
        ValueRef::Blob(bytes) => Ok(SchemaRow::Bound(BoundSchemaRow {
            name,
            role_owner: decode_identity(bytes)?,
            acl: acl.map(serde_json::from_str).transpose()?,
        })),
        _ => Err(SQLiteError::StorageBackend(
            "schema owner has an invalid storage class".into(),
        )),
    }
}

pub(super) fn encode_relation(
    row: &RelationSecurityRow,
) -> Result<(Value, Option<String>, String)> {
    match row {
        RelationSecurityRow::Bound(row) => Ok((
            encode_identity(row.role_owner)?,
            row.acl.as_ref().map(serde_json::to_string).transpose()?,
            serde_json::to_string(&row.column_acls)?,
        )),
        RelationSecurityRow::Legacy(row) => Ok((
            Value::Text(row.role_owner.clone()),
            row.acl.as_ref().map(serde_json::to_string).transpose()?,
            serde_json::to_string(&row.column_acls)?,
        )),
    }
}

pub(super) fn decode_relation(
    owner: ValueRef<'_>,
    acl: Option<&str>,
    columns: Option<&str>,
) -> Result<RelationSecurityRow> {
    match owner {
        ValueRef::Text(_) => Ok(RelationSecurityRow::Legacy(LegacyRelationSecurity {
            role_owner: super::native::string(owner)?,
            acl: acl.map(serde_json::from_str).transpose()?,
            column_acls: columns
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or_default(),
        })),
        ValueRef::Blob(bytes) => Ok(RelationSecurityRow::Bound(BoundRelationSecurity {
            role_owner: decode_identity(bytes)?,
            acl_revisions: uqa_core::catalog_acl::RelationAclRevisions::default(),
            acl: acl.map(serde_json::from_str).transpose()?,
            column_acls: serde_json::from_str(columns.ok_or_else(|| {
                SQLiteError::StorageBackend("bound relation security has no column ACL map".into())
            })?)?,
        })),
        _ => Err(SQLiteError::StorageBackend(
            "relation owner has an invalid storage class".into(),
        )),
    }
}

pub(super) fn decode_relation_cells(
    owner: ValueRef<'_>,
    acl: ValueRef<'_>,
    columns: ValueRef<'_>,
) -> Result<RelationSecurityRow> {
    fn optional_text(value: ValueRef<'_>) -> Result<Option<&str>> {
        if value == ValueRef::Null {
            return Ok(None);
        }
        value.as_str().map(Some).map_err(|_| {
            SQLiteError::StorageBackend("relation ACL has an invalid text storage class".into())
        })
    }
    decode_relation(owner, optional_text(acl)?, optional_text(columns)?)
}

#[cfg(test)]
mod tests;
