//! CREATE/ALTER SEQUENCE lowering and option validation.

use super::{range_var_name, validate_durable_create_relation, NodeEnum, Result, SQLError};

pub(super) fn compile_create_sequence(
    stmt: &pg_query::protobuf::CreateSeqStmt,
) -> Result<crate::ast::CreateSequence> {
    use crate::ast::CreateSequence;
    let relation = stmt
        .sequence
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE SEQUENCE without name".into()))?;
    validate_durable_create_relation(relation, "CREATE SEQUENCE")?;
    if stmt.owner_id != 0 || stmt.for_identity {
        return Err(SQLError::Unsupported(
            "CREATE SEQUENCE: identity-owned sequences are not supported".into(),
        ));
    }
    let name = range_var_name(relation);
    let mut start = None;
    let mut increment = 1_i64;
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(
                "CREATE SEQUENCE contains a malformed option".into(),
            ));
        };
        let key = elem.defname.to_ascii_lowercase();
        let value = compile_sequence_integer_option(elem, "CREATE SEQUENCE")?;
        match key.as_str() {
            "start" => start = Some(value),
            "increment" => increment = value,
            other => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE SEQUENCE option `{other}` is not supported"
                )));
            }
        }
    }
    Ok(CreateSequence {
        name,
        if_not_exists: stmt.if_not_exists,
        // With the unsupported MINVALUE/MAXVALUE clauses excluded above,
        // the SQL defaults are 1 for ascending sequences and -1 for
        // descending sequences.
        start: start.unwrap_or(if increment > 0 { 1 } else { -1 }),
        increment,
    })
}

pub(super) fn compile_alter_sequence(
    stmt: &pg_query::protobuf::AlterSeqStmt,
) -> Result<crate::ast::AlterSequence> {
    use crate::ast::{AlterSequence, SequenceRestart};
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
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(
                "ALTER SEQUENCE contains a malformed option".into(),
            ));
        };
        let key = elem.defname.to_ascii_lowercase();
        let value = if elem.arg.is_none() && key == "restart" {
            None
        } else {
            Some(compile_sequence_integer_option(elem, "ALTER SEQUENCE")?)
        };
        match key.as_str() {
            "restart" => {
                alter.restart = value.map_or(SequenceRestart::FromStart, SequenceRestart::With);
            }
            "increment" => alter.increment = value,
            "start" => alter.start = value,
            other => {
                return Err(SQLError::Unsupported(format!(
                    "ALTER SEQUENCE option `{other}` is not supported"
                )));
            }
        }
    }
    Ok(alter)
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
        SQLError::TypeMismatch(format!(
            "{statement} option `{}` expects an integer, got `{raw}`",
            elem.defname
        ))
    })
}
