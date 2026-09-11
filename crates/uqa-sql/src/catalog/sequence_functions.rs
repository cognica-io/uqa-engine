//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered argument and owner-column binding for sequence introspection functions.

use crate::{
    schema::sequences::ownership::{SequenceOwnerCatalog, SequenceOwnerColumnIdentity},
    SQLError,
};
use uqa_core::Value;

pub fn strict_sequence_oid(function: &str, arguments: &[Value]) -> Result<Option<i64>, SQLError> {
    let [argument] = arguments else {
        return Err(SQLError::BadArity {
            name: function.into(),
            expected: "1".into(),
            actual: arguments.len(),
        });
    };
    match argument {
        Value::Null => Ok(None),
        Value::Int(oid) => Ok(Some(*oid)),
        value => Err(SQLError::TypeMismatch(format!(
            "{function} requires oid or regclass, got {value:?}"
        ))),
    }
}

pub fn strict_sequence_regclass_oid(
    function: &str,
    arguments: &[Value],
    resolve: &mut dyn FnMut(&str) -> Result<Option<i64>, SQLError>,
) -> Result<Option<i64>, SQLError> {
    let [argument] = arguments else {
        return Err(SQLError::BadArity {
            name: function.into(),
            expected: "1".into(),
            actual: arguments.len(),
        });
    };
    match argument {
        Value::Null => Ok(None),
        Value::Int(oid) => Ok(Some(*oid)),
        Value::Str(name) | Value::FixedChar(name) => {
            resolve(name)?.map(Some).ok_or_else(|| SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{name}\" does not exist"),
            })
        }
        value => Err(SQLError::TypeMismatch(format!(
            "{function} requires regclass, got {value:?}"
        ))),
    }
}

pub fn serial_sequence_owner(
    catalog: &dyn SequenceOwnerCatalog,
    arguments: &[Value],
) -> Result<Option<SequenceOwnerColumnIdentity>, SQLError> {
    if arguments.len() != 2 {
        return Err(SQLError::BadArity {
            name: "pg_get_serial_sequence".into(),
            expected: "2".into(),
            actual: arguments.len(),
        });
    }
    if arguments
        .iter()
        .any(|argument| matches!(argument, Value::Null))
    {
        return Ok(None);
    }
    let relation_name = match &arguments[0] {
        Value::Str(value) | Value::FixedChar(value) => value,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "pg_get_serial_sequence table name must be text, got {other:?}"
            )))
        }
    };
    let column_name = match &arguments[1] {
        Value::Str(value) | Value::FixedChar(value) => value,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "pg_get_serial_sequence column name must be text, got {other:?}"
            )))
        }
    };
    let Some((canonical, kind)) = catalog.resolve_owner_relation(relation_name)? else {
        return Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: format!("relation \"{relation_name}\" does not exist"),
        });
    };
    let Some(owner_column) = crate::schema::sequences::ownership::sequence_owner_column_identity(
        catalog,
        &canonical,
        kind,
        column_name,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(owner_column))
}
