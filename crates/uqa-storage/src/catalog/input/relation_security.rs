//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The original format discriminator selects bound identities or legacy role names.

use super::strings::{invalid, json_error, next, take};
use crate::{
    catalog::relation_security::{BoundRelationSecurity, LegacyRelationSecurity},
    read_control::StorageReadControl,
    RelationSecurityRow, StorageBackendResult,
};
use uqa_core::{
    catalog_acl::{input::projected, RelationAclRevisions},
    json::{decode_json_string, JsonReader, JsonToken},
    memory::{Budgeted, OwnedMap},
};

pub(crate) fn read(
    fields: &OwnedMap<String, &[u8]>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<RelationSecurityRow>> {
    if let Some(version) = fields.get("relation_security_format") {
        if serde_json::from_slice::<u64>(version).ok() != Some(1) {
            return Err(invalid("unsupported relation security format"));
        }
        bound(fields, control)
    } else {
        legacy(fields, control)
    }
}

fn bound(
    fields: &OwnedMap<String, &[u8]>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<RelationSecurityRow>> {
    let mut owner = None;
    let mut acl = None;
    let mut columns = None;
    for (name, bytes) in fields {
        control.check()?;
        match name.as_str() {
            "relation_security_format" => (),
            "role_owner" => {
                owner = Some(
                    projected::role_identity(bytes, control.memory(), control.cancellation())
                        .map_err(json_error)?,
                );
            }
            "acl" => {
                acl = Some(
                    projected::entries(bytes, control.memory(), control.cancellation())
                        .map_err(json_error)?,
                );
            }
            "column_acls" => {
                columns = Some(
                    projected::column_entries(bytes, control.memory(), control.cancellation())
                        .map_err(json_error)?,
                );
            }
            _ => return Err(invalid(format!("unknown field `{name}`"))),
        }
    }
    let role_owner = owner.ok_or_else(|| invalid("missing field `role_owner`"))?;
    let acl = acl.ok_or_else(|| invalid("missing field `acl`"))?;
    let columns = columns.ok_or_else(|| invalid("missing field `column_acls`"))?;
    let mut memory = control.memory().empty_reservation();
    let security = BoundRelationSecurity {
        role_owner,
        acl: take(acl, &mut memory),
        column_acls: take(columns, &mut memory),
        acl_revisions: RelationAclRevisions::default(),
    };
    Ok(Budgeted::new(RelationSecurityRow::Bound(security), memory))
}

fn legacy(
    fields: &OwnedMap<String, &[u8]>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<RelationSecurityRow>> {
    let mut owner = None;
    let mut acl = Budgeted::new(None, control.memory().empty_reservation());
    let mut columns = Budgeted::new(OwnedMap::new(), control.memory().empty_reservation());
    for (name, bytes) in fields {
        control.check()?;
        match name.as_str() {
            "role_owner" => {
                let mut reader =
                    JsonReader::from_slice(bytes, control.memory(), control.cancellation());
                owner = Some(text(next(&mut reader)?, control)?);
            }
            "acl" => {
                acl = projected::legacy_entries(bytes, control.memory(), control.cancellation())
                    .map_err(json_error)?;
            }
            "column_acls" => {
                columns = projected::legacy_column_entries(
                    bytes,
                    control.memory(),
                    control.cancellation(),
                )
                .map_err(json_error)?;
            }
            _ => return Err(invalid(format!("unknown field `{name}`"))),
        }
    }
    let owner = owner.ok_or_else(|| invalid("missing field `role_owner`"))?;
    let mut memory = control.memory().empty_reservation();
    let security = LegacyRelationSecurity {
        role_owner: take(owner, &mut memory),
        acl: take(acl, &mut memory),
        column_acls: take(columns, &mut memory),
    };
    Ok(Budgeted::new(RelationSecurityRow::Legacy(security), memory))
}

fn text(
    token: JsonToken<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<String>> {
    let JsonToken::String(value) = token else {
        return Err(invalid("expected a table catalog string"));
    };
    decode_json_string(value, control.memory(), control.cancellation()).map_err(json_error)
}
