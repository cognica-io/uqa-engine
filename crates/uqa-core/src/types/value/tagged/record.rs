//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve opaque named-record identity while decoding legacy and current tagged values.

use super::{int_field, take_list, BTreeMap, Value, ValueRetentionError, Workspace};

pub(super) fn decoded(
    map: &mut BTreeMap<String, Value>,
    workspace: &mut Workspace<'_>,
) -> Result<Option<crate::RecordValue>, ValueRetentionError> {
    let type_oid = if map.contains_key("type_oid") {
        let Some(oid) = int_field::<u32>(map, "type_oid") else {
            return Ok(None);
        };
        Some(oid)
    } else {
        None
    };
    let Some(Value::List(encoded_fields)) = map.get("fields") else {
        return Ok(None);
    };
    for encoded in encoded_fields {
        workspace.check()?;
        if !matches!(encoded, Value::List(pair) if matches!(pair.as_slice(), [Value::Str(_), _])) {
            return Ok(None);
        }
    }
    let mut names = workspace.vector(encoded_fields.len())?;
    let mut values = workspace.vector(encoded_fields.len())?;
    for encoded in take_list(map, "fields") {
        workspace.check()?;
        let Value::List(mut pair) = encoded else {
            unreachable!("record pair was validated");
        };
        let value = pair.pop().expect("validated record value");
        let Some(Value::Str(name)) = pair.pop() else {
            unreachable!("record name was validated");
        };
        names.push(name);
        values.push(value);
    }
    workspace.reserve(crate::RecordValue::retained_header_bytes())?;
    workspace.reserve(crate::RecordDescriptor::buffer_bytes(names.len())?)?;
    let descriptor = crate::RecordDescriptor::new(names, type_oid);
    Ok(Some(crate::RecordValue::from_validated_parts(
        values, descriptor,
    )))
}
