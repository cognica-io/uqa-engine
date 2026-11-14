//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row descriptors decode beside their values under the same input allowance.

use super::{int_field, take_list, BTreeMap, Value, ValueRetentionError, Workspace};
use crate::{RecordFieldType, RowValue};

pub(super) async fn decoded<W: super::Work>(
    map: &mut BTreeMap<String, Value>,
    workspace: &mut Workspace<'_>,
    work: &mut W,
) -> Result<Option<RowValue>, W::Error> {
    let Some(Value::List(values)) = map.get("values") else {
        return if map.contains_key("field_types") {
            Err(malformed("values are not a sequence").into())
        } else {
            Ok(None)
        };
    };
    let field_types = match map.get("field_types") {
        Some(Value::List(types)) => {
            RowValue::validate_width(values.len(), types.len())?;
            let mut decoded = workspace.vector(types.len())?;
            for ty in types {
                workspace.checkpoint(work).await?;
                let Value::Map(ty) = ty else {
                    return Err(malformed("field type is not an object").into());
                };
                if ty
                    .keys()
                    .any(|name| !matches!(name.as_str(), "oid" | "type_modifier" | "collation_oid"))
                {
                    return Err(malformed("field type contains unknown metadata").into());
                }
                let oid = int_field(ty, "oid")
                    .ok_or_else(|| malformed("field type OID is not an unsigned 32-bit integer"))?;
                let type_modifier = int_field(ty, "type_modifier").ok_or_else(|| {
                    malformed("field type modifier is not a signed 32-bit integer")
                })?;
                let collation_oid = match ty.get("collation_oid") {
                    None | Some(Value::Null) => None,
                    Some(_) => Some(int_field(ty, "collation_oid").ok_or_else(|| {
                        malformed("field collation OID is not an unsigned 32-bit integer")
                    })?),
                };
                decoded.push(RecordFieldType {
                    oid,
                    type_modifier,
                    collation_oid,
                });
            }
            Some(decoded)
        }
        Some(Value::Null) | None => None,
        Some(_) => return Err(malformed("field types are not a sequence").into()),
    };
    if let Some(types) = &field_types {
        // Arc slices receive a compact immutable allocation; the intermediate
        // typed vector stays admitted until conversion releases it.
        workspace.reserve(
            crate::RowDescriptor::buffer_bytes(types.len()).map_err(ValueRetentionError::from)?,
        )?;
    }
    workspace.reserve(RowValue::decoded_header_bytes())?;
    workspace.checkpoint(work).await?;
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
