//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read the options of a new sequence as `PostgreSQL`'s `init_params` reads them.

use super::definition::SequenceDefinition;
use crate::ast::{
    ColumnType, SequenceDataType, SequenceDeclaration, SequenceOptionValue, SequenceOwnership,
};
use crate::expr::integer_input::{parse_int8, IntegerInputError};
use crate::SQLError;

/// A new sequence's definition, with the value its first `nextval` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclaredSequence {
    pub definition: SequenceDefinition,
    /// The value the first `nextval` returns: the start, or the value `RESTART` gives.
    pub current: i64,
}

/// Read the options of a new sequence of `data_type`, its written `AS` type or the type of the identity column it serves, in `PostgreSQL`'s order: the type, the increment, cycling, the maximum, the minimum, their ranges, the start, the restart and the cache, each checked as it is read. An omitted option takes the default its type and the direction of its increment give it.
pub fn declare_sequence(
    declaration: &SequenceDeclaration,
    data_type: &ColumnType,
    identity: bool,
) -> Result<DeclaredSequence, SQLError> {
    let data_type = sequence_data_type(data_type, identity)?;
    let increment = match &declaration.increment {
        Some(value) => {
            let increment = sequence_option_integer("increment", value)?;
            if increment == 0 {
                return Err(invalid("INCREMENT must not be zero".into()));
            }
            increment
        }
        None => 1,
    };
    let cycle = declaration.cycle.unwrap_or(false);
    let (type_min, type_max) = data_type.bounds();
    let max_value = match &declaration.max_value {
        Some(value) if *value != SequenceOptionValue::Absent => {
            sequence_option_integer("maxvalue", value)?
        }
        _ if increment > 0 => type_max,
        _ => -1,
    };
    if !(type_min..=type_max).contains(&max_value) {
        return Err(invalid(format!(
            "MAXVALUE ({max_value}) is out of range for sequence data type {}",
            data_type.sql_name()
        )));
    }
    let min_value = match &declaration.min_value {
        Some(value) if *value != SequenceOptionValue::Absent => {
            sequence_option_integer("minvalue", value)?
        }
        _ if increment < 0 => type_min,
        _ => 1,
    };
    if !(type_min..=type_max).contains(&min_value) {
        return Err(invalid(format!(
            "MINVALUE ({min_value}) is out of range for sequence data type {}",
            data_type.sql_name()
        )));
    }
    if min_value >= max_value {
        return Err(invalid(format!(
            "MINVALUE ({min_value}) must be less than MAXVALUE ({max_value})"
        )));
    }
    let start = match &declaration.start {
        Some(value) => sequence_option_integer("start", value)?,
        None if increment > 0 => min_value,
        None => max_value,
    };
    if start < min_value {
        return Err(invalid(format!(
            "START value ({start}) cannot be less than MINVALUE ({min_value})"
        )));
    }
    if start > max_value {
        return Err(invalid(format!(
            "START value ({start}) cannot be greater than MAXVALUE ({max_value})"
        )));
    }
    let current = match &declaration.restart {
        None | Some(SequenceOptionValue::Absent) => start,
        Some(value) => sequence_option_integer("restart", value)?,
    };
    if current < min_value {
        return Err(invalid(format!(
            "RESTART value ({current}) cannot be less than MINVALUE ({min_value})"
        )));
    }
    if current > max_value {
        return Err(invalid(format!(
            "RESTART value ({current}) cannot be greater than MAXVALUE ({max_value})"
        )));
    }
    let cache_size = match &declaration.cache {
        Some(value) => {
            let cache_size = sequence_option_integer("cache", value)?;
            if cache_size <= 0 {
                return Err(invalid(format!(
                    "CACHE ({cache_size}) must be greater than zero"
                )));
            }
            cache_size
        }
        None => 1,
    };
    Ok(DeclaredSequence {
        definition: SequenceDefinition {
            start,
            increment,
            data_type,
            min_value,
            max_value,
            cycle,
            cache_size,
        },
        current,
    })
}

/// The column `OWNED BY` names, as `PostgreSQL` reads it once it has created the sequence: `NONE`, or a relation and one of its columns.
pub fn sequence_ownership(names: &[String]) -> Result<SequenceOwnership, SQLError> {
    use crate::compiler::render_relation_component;
    match names {
        [none] if none == "none" => Ok(SequenceOwnership::Unowned),
        [] | [_] => Err(SQLError::Diagnostic {
            sqlstate: "42601".into(),
            message: "invalid OWNED BY option".into(),
            detail: None,
            hint: Some("Specify OWNED BY table.column or OWNED BY NONE.".into()),
        }),
        [table, column] => Ok(SequenceOwnership::Column {
            table: render_relation_component(table),
            column: column.clone(),
        }),
        [schema, table, column] => Ok(SequenceOwnership::Column {
            table: format!(
                "{}.{}",
                render_relation_component(schema),
                render_relation_component(table)
            ),
            column: column.clone(),
        }),
        _ => Err(SQLError::Unsupported(
            "cross-database references are not implemented: OWNED BY".into(),
        )),
    }
}

/// The integer type a sequence counts in. An identity column's sequence counts in its column's type.
fn sequence_data_type(
    data_type: &ColumnType,
    identity: bool,
) -> Result<SequenceDataType, SQLError> {
    match data_type {
        ColumnType::SmallInteger => Ok(SequenceDataType::SmallInt),
        ColumnType::Integer => Ok(SequenceDataType::Integer),
        ColumnType::BigInteger => Ok(SequenceDataType::BigInt),
        _ => Err(invalid(
            if identity {
                "identity column type must be smallint, integer, or bigint"
            } else {
                "sequence type must be smallint, integer, or bigint"
            }
            .into(),
        )),
    }
}

/// An option's value as `PostgreSQL`'s `defGetInt64` reads it.
pub fn sequence_option_integer(name: &str, value: &SequenceOptionValue) -> Result<i64, SQLError> {
    match value {
        SequenceOptionValue::Integer(value) => Ok(*value),
        SequenceOptionValue::Text(text) => parse_int8(text).map_err(|error| match error {
            IntegerInputError::OutOfRange => SQLError::Routine {
                sqlstate: "22003".into(),
                message: format!("value \"{text}\" is out of range for type bigint"),
            },
            IntegerInputError::InvalidSyntax => SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type bigint: \"{text}\""),
            },
        }),
        SequenceOptionValue::Absent => Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: format!("{name} requires a numeric value"),
        }),
    }
}

fn invalid(message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message,
    }
}

#[cfg(test)]
mod tests;
