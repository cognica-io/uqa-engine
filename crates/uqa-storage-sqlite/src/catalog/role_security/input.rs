//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled native authority cells preserve storage-class selection and original field order.

mod sequence;
pub(in crate::catalog) use sequence::read as sequence;

use super::{
    decode_identity, optional_text, BoundRelationSecurity, LegacyRelationSecurity, Result,
    SQLiteError,
};
use rusqlite::types::ValueRef;
use serde::de::Error as _;
use uqa_core::{
    catalog_acl::{input, RelationAclRevisions},
    json::JsonReadError,
    memory::{Budgeted, MemoryReservation, OwnedMap, ProductionControl},
};
use uqa_storage::{read_control::StorageReadControl, RelationSecurityRow};

pub(in crate::catalog) fn schema(
    name: &str,
    owner: ValueRef<'_>,
    acl: Option<&str>,
    control: &StorageReadControl,
) -> Result<Budgeted<uqa_storage::SchemaRow>> {
    use uqa_core::catalog_schema::{BoundSchemaRow, SchemaRow as LegacySchemaRow};
    use uqa_storage::SchemaRow;
    control.check()?;
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let text = |name: &str| -> Result<Budgeted<String>> {
        Ok(production
            .copy_text(name)
            .map_err(uqa_storage::StorageBackendError::from)?
            .into_budgeted()
            .expect("admitted schema text"))
    };
    let name = text(name)?;
    let mut memory = control.memory().empty_reservation();
    let row = match owner {
        ValueRef::Text(_) => {
            let owner = text(super::super::native::string_ref(owner)?)?;
            let acl = acl
                .map(|text| {
                    nonnull(
                        input::schema::legacy_entries(
                            text.as_bytes(),
                            control.memory(),
                            control.cancellation(),
                        )
                        .map_err(json_error)?,
                    )
                })
                .transpose()?
                .unwrap_or_else(|| Budgeted::new(None, control.memory().empty_reservation()));
            SchemaRow::Legacy(LegacySchemaRow {
                name: take(name, &mut memory),
                role_owner: take(owner, &mut memory),
                acl: take(acl, &mut memory),
            })
        }
        ValueRef::Blob(bytes) => {
            let (role_owner, tuple) = super::decode_schema_owner(bytes)?;
            let acl = acl
                .map(|text| {
                    nonnull(
                        input::schema::entries(
                            text.as_bytes(),
                            control.memory(),
                            control.cancellation(),
                        )
                        .map_err(json_error)?,
                    )
                })
                .transpose()?
                .unwrap_or_else(|| Budgeted::new(None, control.memory().empty_reservation()));
            SchemaRow::Bound(BoundSchemaRow {
                name: take(name, &mut memory),
                tuple,
                role_owner,
                acl: take(acl, &mut memory),
            })
        }
        _ => {
            return Err(SQLiteError::StorageBackend(
                "schema owner has an invalid storage class".into(),
            ))
        }
    };
    let row = Budgeted::new(row, memory);
    control.check()?;
    Ok(row)
}

pub(in crate::catalog) fn relation(
    owner: ValueRef<'_>,
    acl: ValueRef<'_>,
    columns: ValueRef<'_>,
    control: &StorageReadControl,
) -> Result<Budgeted<RelationSecurityRow>> {
    control.check()?;
    let acl = optional_text(acl)?;
    let columns = optional_text(columns)?;
    let mut memory = control.memory().empty_reservation();
    match owner {
        ValueRef::Text(_) => {
            let production = ProductionControl::new(
                control.memory(),
                control.cancellation(),
                control.cancellation(),
            );
            let owner = production
                .copy_text(super::super::native::string_ref(owner)?)
                .map_err(uqa_storage::StorageBackendError::from)?
                .into_budgeted()
                .expect("controlled owner name");
            let acl = acl
                .map(|text| {
                    nonnull(
                        input::legacy_entries(
                            text.as_bytes(),
                            control.memory(),
                            control.cancellation(),
                        )
                        .map_err(json_error)?,
                    )
                })
                .transpose()?
                .unwrap_or_else(|| Budgeted::new(None, control.memory().empty_reservation()));
            let columns = columns
                .map(|text| {
                    input::legacy_column_entries(
                        text.as_bytes(),
                        control.memory(),
                        control.cancellation(),
                    )
                    .map_err(json_error)
                })
                .transpose()?
                .unwrap_or_else(|| {
                    Budgeted::new(OwnedMap::new(), control.memory().empty_reservation())
                });
            control.check()?;
            Ok(Budgeted::new(
                RelationSecurityRow::Legacy(LegacyRelationSecurity {
                    role_owner: take(owner, &mut memory),
                    acl: take(acl, &mut memory),
                    column_acls: take(columns, &mut memory),
                }),
                memory,
            ))
        }
        ValueRef::Blob(bytes) => {
            let owner = decode_identity(bytes)?;
            let acl = acl
                .map(|text| {
                    nonnull(
                        input::entries(text.as_bytes(), control.memory(), control.cancellation())
                            .map_err(json_error)?,
                    )
                })
                .transpose()?
                .unwrap_or_else(|| Budgeted::new(None, control.memory().empty_reservation()));
            let columns = columns.ok_or_else(|| {
                SQLiteError::StorageBackend("bound relation security has no column ACL map".into())
            })?;
            let columns =
                input::column_entries(columns.as_bytes(), control.memory(), control.cancellation())
                    .map_err(json_error)?;
            control.check()?;
            Ok(Budgeted::new(
                RelationSecurityRow::Bound(BoundRelationSecurity {
                    role_owner: owner,
                    acl: take(acl, &mut memory),
                    column_acls: take(columns, &mut memory),
                    acl_revisions: RelationAclRevisions::default(),
                }),
                memory,
            ))
        }
        _ => Err(SQLiteError::StorageBackend(
            "relation owner has an invalid storage class".into(),
        )),
    }
}

fn nonnull<T>(value: Budgeted<Option<T>>) -> Result<Budgeted<Option<T>>> {
    if value.is_none() {
        return Err(SQLiteError::Serde(serde_json::Error::custom(
            "invalid type: null, expected a sequence",
        )));
    }
    Ok(value)
}

pub(in crate::catalog) fn take<T>(value: Budgeted<T>, memory: &mut MemoryReservation) -> T {
    let (value, lease) = value.into_parts();
    memory.absorb(lease);
    value
}

fn json_error(error: JsonReadError) -> SQLiteError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        error => SQLiteError::Serde(serde_json::Error::custom(error)),
    }
}

#[cfg(test)]
mod tests;
