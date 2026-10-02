//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE/ALTER SEQUENCE lowering and option validation.

use super::{
    compile_pg_type_name, extract_string, range_var_name, relation_persistence, NodeEnum, Result,
    SQLError,
};
use crate::ast::{
    ColumnType, DeferredSQLError, IdentitySequenceDeclaration, IdentitySequenceName,
    RelationPersistence, SequenceDeclaration, SequenceOptionValue,
};
use crate::schema::sequences::declaration::{
    declare_sequence, sequence_option_integer, sequence_ownership,
};
use pg_query::protobuf::{DefElem, Node};

pub(super) fn compile_create_sequence(
    stmt: &pg_query::protobuf::CreateSeqStmt,
) -> Result<crate::ast::CreateSequence> {
    use crate::ast::{CreateSequence, SequenceRestart};
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
    let declaration = collect_sequence_options(
        option_elements(&stmt.options, "CREATE SEQUENCE")?,
        "CREATE SEQUENCE",
        false,
    )?;
    let declared = declare_sequence(
        &declaration,
        declaration
            .data_type
            .as_ref()
            .unwrap_or(&ColumnType::BigInteger),
        false,
    )?;
    let ownership = declaration
        .owned_by
        .as_deref()
        .map(sequence_ownership)
        .transpose()?
        .unwrap_or_default();
    let definition = declared.definition;
    Ok(CreateSequence {
        name,
        if_not_exists: stmt.if_not_exists,
        start: definition.start,
        increment: definition.increment,
        persistence,
        data_type: definition.data_type,
        min_value: Some(definition.min_value),
        max_value: Some(definition.max_value),
        cycle: definition.cycle,
        cache_size: definition.cache_size,
        ownership,
        restart: declaration
            .restart
            .as_ref()
            .map_or(SequenceRestart::Unchanged, |_| {
                SequenceRestart::With(declared.current)
            }),
    })
}

/// The sequence options of an identity column declaration, `GENERATED ... AS IDENTITY (options)`. `PostgreSQL` takes `SEQUENCE NAME`, `LOGGED` and `UNLOGGED` while it analyzes the statement, where repeating one conflicts, and reads the others when it creates the sequence, so an error in those waits until then.
pub(super) fn compile_identity_declaration(
    options: &[Node],
) -> Result<Option<Box<IdentitySequenceDeclaration>>> {
    if options.is_empty() {
        return Ok(None);
    }
    let mut declaration = IdentitySequenceDeclaration::default();
    let mut sequence_options = Vec::with_capacity(options.len());
    let mut named = false;
    let mut persisted = false;
    for elem in option_elements(options, "identity column")? {
        match elem.defname.as_str() {
            "sequence_name" => {
                if std::mem::replace(&mut named, true) {
                    return Err(conflicting_sequence_options());
                }
                declaration.name = Some(identity_sequence_name(elem)?);
            }
            "logged" | "unlogged" => {
                if std::mem::replace(&mut persisted, true) {
                    return Err(conflicting_sequence_options());
                }
                declaration.persistence = Some(if elem.defname == "logged" {
                    RelationPersistence::Permanent
                } else {
                    RelationPersistence::Unlogged
                });
            }
            _ => sequence_options.push(elem),
        }
    }
    match collect_sequence_options(sequence_options, "CREATE SEQUENCE", true) {
        Ok(sequence) => declaration.sequence = sequence,
        Err(error) => declaration.error = Some(DeferredSQLError::from(&error)),
    }
    Ok(Some(Box::new(declaration)))
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
    for elem in option_elements(&stmt.options, "ALTER SEQUENCE")? {
        let key = elem.defname.to_ascii_lowercase();
        reject_unknown_sequence_option(&key)?;
        if !seen.insert(key.clone()) {
            return Err(conflicting_sequence_options());
        }
        match key.as_str() {
            "restart" => {
                alter.restart = match sequence_option_value(elem)? {
                    SequenceOptionValue::Absent => SequenceRestart::FromStart,
                    value => SequenceRestart::With(sequence_option_integer("restart", &value)?),
                };
            }
            "increment" => alter.increment = Some(compile_sequence_integer_option(elem)?),
            "start" => alter.start = Some(compile_sequence_integer_option(elem)?),
            "as" => alter.data_type = Some(compile_sequence_data_type(elem, "ALTER SEQUENCE")?),
            "minvalue" => {
                alter.min_value = elem.arg.as_ref().map_or(Ok(SequenceBound::Default), |_| {
                    compile_sequence_integer_option(elem).map(SequenceBound::Value)
                })?;
            }
            "maxvalue" => {
                alter.max_value = elem.arg.as_ref().map_or(Ok(SequenceBound::Default), |_| {
                    compile_sequence_integer_option(elem).map(SequenceBound::Value)
                })?;
            }
            "cycle" => {
                alter.cycle = Some(compile_sequence_boolean_option(elem, "ALTER SEQUENCE")?);
            }
            "cache" => alter.cache_size = Some(compile_sequence_integer_option(elem)?),
            "owned_by" => alter.ownership = sequence_ownership(&name_list(elem)?)?,
            _ => unreachable!("unknown sequence options were rejected above"),
        }
    }
    Ok(alter)
}

/// Collect sequence options in their written order, as `PostgreSQL`'s `init_params` does before it reads their values: repeating an option conflicts with its first occurrence. An identity column's sequence counts in its column's type, which `PostgreSQL` supplies as a first `AS`, so a written `AS` conflicts with it.
fn collect_sequence_options<'a>(
    elements: impl IntoIterator<Item = &'a DefElem>,
    statement: &str,
    identity: bool,
) -> Result<SequenceDeclaration> {
    let mut declaration = SequenceDeclaration::default();
    let mut seen = std::collections::BTreeSet::new();
    if identity {
        seen.insert("as".to_string());
    }
    for elem in elements {
        let key = elem.defname.to_ascii_lowercase();
        reject_unknown_sequence_option(&key)?;
        if !seen.insert(key.clone()) {
            return Err(conflicting_sequence_options());
        }
        match key.as_str() {
            "as" => {
                declaration.data_type = Some(sequence_type_name(elem, statement)?);
            }
            "increment" => declaration.increment = Some(sequence_option_value(elem)?),
            "cycle" => {
                declaration.cycle = Some(compile_sequence_boolean_option(elem, statement)?);
            }
            "maxvalue" => declaration.max_value = Some(sequence_option_value(elem)?),
            "minvalue" => declaration.min_value = Some(sequence_option_value(elem)?),
            "start" => declaration.start = Some(sequence_option_value(elem)?),
            "restart" => declaration.restart = Some(sequence_option_value(elem)?),
            "cache" => declaration.cache = Some(sequence_option_value(elem)?),
            "owned_by" => declaration.owned_by = Some(name_list(elem)?),
            _ => unreachable!("unknown sequence options were rejected above"),
        }
    }
    Ok(declaration)
}

/// Reject an option `PostgreSQL`'s `init_params` does not read. The parser accepts `SEQUENCE NAME`, `LOGGED` and `UNLOGGED` for identity declarations only, which take them before these options reach it.
fn reject_unknown_sequence_option(key: &str) -> Result<()> {
    match key {
        "as" | "increment" | "cycle" | "maxvalue" | "minvalue" | "start" | "restart" | "cache"
        | "owned_by" => Ok(()),
        "sequence_name" => Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "invalid sequence option SEQUENCE NAME".into(),
        }),
        other => Err(SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!("option \"{other}\" not recognized"),
        }),
    }
}

fn option_elements<'a>(options: &'a [Node], statement: &str) -> Result<Vec<&'a DefElem>> {
    options
        .iter()
        .map(|option| match option.node.as_ref() {
            Some(NodeEnum::DefElem(elem)) => Ok(elem.as_ref()),
            _ => Err(SQLError::Internal(format!(
                "{statement} contains a malformed option"
            ))),
        })
        .collect()
}

/// The name `SEQUENCE NAME` writes: a sequence name, optionally qualified by its schema.
fn identity_sequence_name(elem: &DefElem) -> Result<IdentitySequenceName> {
    let names = name_list(elem)?;
    match names.as_slice() {
        [name] => Ok(IdentitySequenceName {
            schema: None,
            name: name.clone(),
        }),
        [schema, name] => Ok(IdentitySequenceName {
            schema: Some(schema.clone()),
            name: name.clone(),
        }),
        [_, _, _] => Err(SQLError::Unsupported(format!(
            "cross-database references are not implemented: \"{}\"",
            names.join(".")
        ))),
        _ => Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: format!(
                "improper relation name (too many dotted names): {}",
                names.join(".")
            ),
        }),
    }
}

/// The names a name-list option writes: `OWNED BY` or `SEQUENCE NAME`.
fn name_list(elem: &DefElem) -> Result<Vec<String>> {
    let Some(NodeEnum::List(list)) = elem
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    else {
        return Err(SQLError::Internal(format!(
            "sequence option `{}` has a malformed name",
            elem.defname
        )));
    };
    list.items.iter().map(extract_string).collect()
}

fn conflicting_sequence_options() -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: "conflicting or redundant options".into(),
    }
}

fn sequence_type_name(elem: &DefElem, statement: &str) -> Result<ColumnType> {
    let Some(NodeEnum::TypeName(type_name)) =
        elem.arg.as_deref().and_then(|node| node.node.as_ref())
    else {
        return Err(SQLError::Internal(format!(
            "{statement} contains a malformed AS type"
        )));
    };
    compile_pg_type_name(type_name, "sequence")
}

fn compile_sequence_data_type(
    elem: &DefElem,
    statement: &str,
) -> Result<crate::ast::SequenceDataType> {
    match sequence_type_name(elem, statement)? {
        ColumnType::SmallInteger => Ok(crate::ast::SequenceDataType::SmallInt),
        ColumnType::Integer => Ok(crate::ast::SequenceDataType::Integer),
        ColumnType::BigInteger => Ok(crate::ast::SequenceDataType::BigInt),
        _ => Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: "sequence type must be smallint, integer, or bigint".into(),
        }),
    }
}

fn compile_sequence_boolean_option(elem: &DefElem, statement: &str) -> Result<bool> {
    match elem.arg.as_deref().and_then(|node| node.node.as_ref()) {
        Some(NodeEnum::Boolean(value)) => Ok(value.boolval),
        Some(NodeEnum::Integer(value)) if matches!(value.ival, 0 | 1) => Ok(value.ival != 0),
        other => Err(SQLError::Internal(format!(
            "{statement} option `{}` has malformed Boolean value {other:?}",
            elem.defname
        ))),
    }
}

/// An option's value as the parser keeps it: a numeric option's value is an integer, or a number the parser kept as written.
fn sequence_option_value(elem: &DefElem) -> Result<SequenceOptionValue> {
    match elem
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    {
        None => Ok(SequenceOptionValue::Absent),
        Some(NodeEnum::Integer(value)) => Ok(SequenceOptionValue::Integer(i64::from(value.ival))),
        Some(NodeEnum::Float(value)) => Ok(SequenceOptionValue::Text(value.fval.clone())),
        Some(NodeEnum::String(value)) => Ok(SequenceOptionValue::Text(value.sval.clone())),
        Some(_) => Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: format!("{} requires a numeric value", elem.defname),
        }),
    }
}

fn compile_sequence_integer_option(elem: &DefElem) -> Result<i64> {
    sequence_option_integer(&elem.defname, &sequence_option_value(elem)?)
}
