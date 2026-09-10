//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL names and declaration metadata for SERIAL and IDENTITY sequences.
use uqa_core::{RelationIdentity, Value};

pub fn stored_owner_names_current(
    table: &RelationIdentity,
    column: &crate::ast::ColumnDef,
    owner: &crate::ast::AutoIncrementOwner,
) -> bool {
    let table_matches =
        RelationIdentity::parse_reference(&owner.table).is_ok_and(|(schema, name)| {
            schema.is_none_or(|schema| schema == table.schema) && name == table.name
        });
    table_matches && owner.column == column.name
}

pub fn implicit_sequence_data_type(
    column: &crate::ast::ColumnDef,
) -> Result<crate::ast::SequenceDataType, String> {
    match &column.ty {
        crate::ast::ColumnType::SmallInteger => Ok(crate::ast::SequenceDataType::SmallInt),
        crate::ast::ColumnType::Integer => Ok(crate::ast::SequenceDataType::Integer),
        crate::ast::ColumnType::BigInteger => Ok(crate::ast::SequenceDataType::BigInt),
        _ => Err(format!(
            "implicit sequence column `{}` has non-integer type",
            column.name
        )),
    }
}

const POSTGRES_IDENTIFIER_MAX_BYTES: usize = 63;

fn clip_identifier_component(value: &str, byte_length: usize) -> &str {
    let mut end = byte_length.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn implicit_sequence_local_name(
    table: &str,
    column: &str,
    collision_pass: usize,
) -> Result<String, String> {
    let label = if collision_pass == 0 {
        "seq".to_string()
    } else {
        format!("seq{collision_pass}")
    };
    let overhead = label.len() + 2;
    let available = POSTGRES_IDENTIFIER_MAX_BYTES
        .checked_sub(overhead)
        .filter(|available| *available > 0)
        .ok_or_else(|| format!("cannot generate an implicit sequence name with label `{label}`"))?;
    let mut table_bytes = table.len();
    let mut column_bytes = column.len();
    while table_bytes + column_bytes > available {
        if table_bytes > column_bytes {
            table_bytes -= 1;
        } else {
            column_bytes -= 1;
        }
    }
    let table = clip_identifier_component(table, table_bytes);
    let column = clip_identifier_component(column, column_bytes);
    Ok(format!("{table}_{column}_{label}"))
}

pub fn choose_implicit_sequence_name<E>(
    table: &RelationIdentity,
    column: &str,
    mut collides: impl FnMut(&RelationIdentity) -> Result<bool, E>,
    invalid_name: impl Fn(String) -> E,
) -> Result<String, E> {
    for collision_pass in 0.. {
        let candidate = RelationIdentity::new(
            table.schema.clone(),
            implicit_sequence_local_name(&table.name, column, collision_pass)
                .map_err(&invalid_name)?,
        );
        if !collides(&candidate)? {
            return Ok(candidate.qualified_name());
        }
    }
    unreachable!("the collision pass is unbounded")
}

pub fn apply_implicit_sequence_metadata(
    table_name: &str,
    column: &mut crate::ast::ColumnDef,
    sequence: String,
) -> Result<(), String> {
    let auto_increment = column.auto_increment.as_mut().ok_or_else(|| {
        format!(
            "implicit sequence column `{table_name}`.`{}` lost its generation metadata",
            column.name
        )
    })?;
    auto_increment.sequence = Some(sequence.clone());
    auto_increment.owner = Some(crate::ast::AutoIncrementOwner {
        table: table_name.to_string(),
        column: column.name.clone(),
    });
    if auto_increment.kind == crate::ast::AutoIncrementKind::Serial {
        column.default = Some(crate::ast::Expr::Func {
            name: "nextval".into(),
            binding: None,
            args: vec![crate::ast::Expr::Literal(Value::Str(sequence))],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        });
    }
    Ok(())
}
