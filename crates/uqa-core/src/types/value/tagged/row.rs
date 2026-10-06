//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row descriptors decode beside their values under the same input allowance.

use super::{int_field, take_list, BTreeMap, Value, ValueRetentionError, Workspace};
use crate::{RecordFieldType, RowValue};

pub(super) fn decoded(
    map: &mut BTreeMap<String, Value>,
    workspace: &mut Workspace<'_>,
) -> Result<Option<RowValue>, ValueRetentionError> {
    let Some(Value::List(values)) = map.get("values") else {
        return if map.contains_key("field_types") {
            Err(malformed("values are not a sequence"))
        } else {
            Ok(None)
        };
    };
    let field_types = match map.get("field_types") {
        Some(Value::List(types)) => {
            RowValue::validate_width(values.len(), types.len())?;
            let mut decoded = workspace.vector(types.len())?;
            for ty in types {
                workspace.check()?;
                let Value::Map(ty) = ty else {
                    return Err(malformed("field type is not an object"));
                };
                if ty.len() != 2 {
                    return Err(malformed("field type must contain oid and type_modifier"));
                }
                let oid = int_field(ty, "oid")
                    .ok_or_else(|| malformed("field type OID is not an unsigned 32-bit integer"))?;
                let type_modifier = int_field(ty, "type_modifier").ok_or_else(|| {
                    malformed("field type modifier is not a signed 32-bit integer")
                })?;
                decoded.push(RecordFieldType { oid, type_modifier });
            }
            Some(decoded)
        }
        Some(Value::Null) | None => None,
        Some(_) => return Err(malformed("field types are not a sequence")),
    };
    workspace.reserve(RowValue::decoded_header_bytes())?;
    workspace.check()?;
    Ok(Some(RowValue::from_validated_parts(
        take_list(map, "values"),
        field_types,
    )))
}

fn malformed(reason: &str) -> ValueRetentionError {
    ValueRetentionError::Malformed {
        kind: "row",
        reason: reason.into(),
    }
}
