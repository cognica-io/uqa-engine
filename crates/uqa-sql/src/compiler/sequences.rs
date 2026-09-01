//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE/ALTER SEQUENCE lowering and option validation.

use super::{
    compile_pg_type_name, range_var_name, relation_persistence, NodeEnum, Result, SQLError,
};
use crate::ast::ColumnType;

pub(super) fn compile_create_sequence(
    stmt: &pg_query::protobuf::CreateSeqStmt,
) -> Result<crate::ast::CreateSequence> {
    use crate::ast::{CreateSequence, SequenceDataType};
    let relation = stmt
        .sequence
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE SEQUENCE without name".into()))?;
    let persistence = relation_persistence(relation, "CREATE SEQUENCE")?;
    if stmt.owner_id != 0 || stmt.for_identity {
        return Err(SQLError::Unsupported(
            "CREATE SEQUENCE: identity-owned sequences are not supported".into(),
        ));
    }
    let name = range_var_name(relation);
    let mut start = None;
    let mut increment = 1_i64;
    let mut data_type = SequenceDataType::BigInt;
    let mut min_value = None;
    let mut max_value = None;
    let mut cycle = false;
    let mut cache_size = 1;
    let mut seen = std::collections::BTreeSet::new();
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(
                "CREATE SEQUENCE contains a malformed option".into(),
            ));
        };
        let key = elem.defname.to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err(conflicting_sequence_options());
        }
        match key.as_str() {
            "as" => data_type = compile_sequence_data_type(elem, "CREATE SEQUENCE")?,
            "start" => start = Some(compile_sequence_integer_option(elem, "CREATE SEQUENCE")?),
            "increment" => {
                increment = compile_sequence_integer_option(elem, "CREATE SEQUENCE")?;
            }
            "minvalue" => {
                min_value = elem
                    .arg
                    .as_ref()
                    .map(|_| compile_sequence_integer_option(elem, "CREATE SEQUENCE"))
                    .transpose()?;
            }
            "maxvalue" => {
                max_value = elem
                    .arg
                    .as_ref()
                    .map(|_| compile_sequence_integer_option(elem, "CREATE SEQUENCE"))
                    .transpose()?;
            }
            "cycle" => cycle = compile_sequence_boolean_option(elem, "CREATE SEQUENCE")?,
            "cache" => {
                cache_size = compile_sequence_integer_option(elem, "CREATE SEQUENCE")?;
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE SEQUENCE option `{other}` is not supported"
                )));
            }
        }
    }
    let (type_min, type_max) = data_type.bounds();
    let min_value = min_value.unwrap_or(if increment > 0 { 1 } else { type_min });
    let max_value = max_value.unwrap_or(if increment > 0 { type_max } else { -1 });
    Ok(CreateSequence {
        name,
        if_not_exists: stmt.if_not_exists,
        start: start.unwrap_or(if increment > 0 { min_value } else { max_value }),
        increment,
        persistence,
        data_type,
        min_value: Some(min_value),
        max_value: Some(max_value),
        cycle,
        cache_size,
    })
}

pub(super) fn compile_alter_sequence(
    stmt: &pg_query::protobuf::AlterSeqStmt,
) -> Result<crate::ast::AlterSequence> {
    use crate::ast::{AlterSequence, SequenceBound, SequenceRestart};
    if stmt.for_identity {
        return Err(SQLError::Unsupported(
            "ALTER SEQUENCE: identity-owned sequences are not supported".into(),
        ));
    }
    let name = stmt
        .sequence
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("ALTER SEQUENCE without name".into()))?;
    let mut alter = AlterSequence {
        name,
        if_exists: stmt.missing_ok,
        ..Default::default()
    };
    let mut seen = std::collections::BTreeSet::new();
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(
                "ALTER SEQUENCE contains a malformed option".into(),
            ));
        };
        let key = elem.defname.to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err(conflicting_sequence_options());
        }
        match key.as_str() {
            "restart" => {
                alter.restart = elem
                    .arg
                    .as_ref()
                    .map(|_| compile_sequence_integer_option(elem, "ALTER SEQUENCE"))
                    .transpose()?
                    .map_or(SequenceRestart::FromStart, SequenceRestart::With);
            }
            "increment" => {
                alter.increment = Some(compile_sequence_integer_option(elem, "ALTER SEQUENCE")?);
            }
            "start" => {
                alter.start = Some(compile_sequence_integer_option(elem, "ALTER SEQUENCE")?);
            }
            "as" => alter.data_type = Some(compile_sequence_data_type(elem, "ALTER SEQUENCE")?),
            "minvalue" => {
                alter.min_value = elem.arg.as_ref().map_or(Ok(SequenceBound::Default), |_| {
                    compile_sequence_integer_option(elem, "ALTER SEQUENCE")
                        .map(SequenceBound::Value)
                })?;
            }
            "maxvalue" => {
                alter.max_value = elem.arg.as_ref().map_or(Ok(SequenceBound::Default), |_| {
                    compile_sequence_integer_option(elem, "ALTER SEQUENCE")
                        .map(SequenceBound::Value)
                })?;
            }
            "cycle" => {
                alter.cycle = Some(compile_sequence_boolean_option(elem, "ALTER SEQUENCE")?);
            }
            "cache" => {
                alter.cache_size = Some(compile_sequence_integer_option(elem, "ALTER SEQUENCE")?);
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "ALTER SEQUENCE option `{other}` is not supported"
                )));
            }
        }
    }
    Ok(alter)
}

fn conflicting_sequence_options() -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: "conflicting or redundant options".into(),
    }
}

fn compile_sequence_data_type(
    elem: &pg_query::protobuf::DefElem,
    statement: &str,
) -> Result<crate::ast::SequenceDataType> {
    let Some(NodeEnum::TypeName(type_name)) =
        elem.arg.as_deref().and_then(|node| node.node.as_ref())
    else {
        return Err(SQLError::Internal(format!(
            "{statement} contains a malformed AS type"
        )));
    };
    match compile_pg_type_name(type_name, "sequence")? {
        ColumnType::SmallInteger => Ok(crate::ast::SequenceDataType::SmallInt),
        ColumnType::Integer => Ok(crate::ast::SequenceDataType::Integer),
        ColumnType::BigInteger => Ok(crate::ast::SequenceDataType::BigInt),
        _ => Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: "sequence type must be smallint, integer, or bigint".into(),
        }),
    }
}

fn compile_sequence_boolean_option(
    elem: &pg_query::protobuf::DefElem,
    statement: &str,
) -> Result<bool> {
    match elem.arg.as_deref().and_then(|node| node.node.as_ref()) {
        Some(NodeEnum::Boolean(value)) => Ok(value.boolval),
        Some(NodeEnum::Integer(value)) if matches!(value.ival, 0 | 1) => Ok(value.ival != 0),
        other => Err(SQLError::Internal(format!(
            "{statement} option `{}` has malformed Boolean value {other:?}",
            elem.defname
        ))),
    }
}

pub(super) fn compile_sequence_integer_option(
    elem: &pg_query::protobuf::DefElem,
    statement: &str,
) -> Result<i64> {
    let raw = match elem
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    {
        Some(NodeEnum::Integer(value)) => return Ok(i64::from(value.ival)),
        Some(NodeEnum::Float(value)) => value.fval.as_str(),
        Some(NodeEnum::String(value)) => value.sval.as_str(),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "{statement} option `{}` expects an integer, got {other:?}",
                elem.defname
            )));
        }
    };
    raw.parse::<i64>().map_err(|_| {
        let integer_syntax = raw
            .strip_prefix(['+', '-'])
            .unwrap_or(raw)
            .bytes()
            .all(|byte| byte.is_ascii_digit());
        SQLError::Routine {
            sqlstate: if integer_syntax { "22003" } else { "22P02" }.into(),
            message: if integer_syntax {
                format!("value \"{raw}\" is out of range for type bigint")
            } else {
                format!("invalid input syntax for type bigint: \"{raw}\"")
            },
        }
    })
}
