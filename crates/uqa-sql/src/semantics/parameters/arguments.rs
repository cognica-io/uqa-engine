//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The value text of `SET name TO argument, ...`, flattened as `PostgreSQL`'s `flatten_set_variable_args` does: only a list parameter takes several arguments, which are joined with `, `, and a string of a quoted list is quoted as an identifier needs.

use super::catalog::find_parameter;
use super::definition::ParameterFlags;
use crate::SQLError;

/// One argument of `SET`, as the grammar delivers it.
#[derive(Clone, Debug, PartialEq)]
pub enum SetArgument {
    /// An integer constant, written in decimal.
    Integer(i64),
    /// A numeric constant that is not an integer, kept as written.
    Number(String),
    /// A string constant, an identifier or a keyword.
    Text(String),
}

/// The value text that `SET name TO arguments` assigns.
pub fn flatten_set_arguments(name: &str, arguments: &[SetArgument]) -> Result<String, SQLError> {
    let flags = find_parameter(name).map_or(ParameterFlags::NONE, |definition| definition.flags);
    if !flags.contains(ParameterFlags::LIST_INPUT) && arguments.len() != 1 {
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("SET {name} takes only one argument"),
        });
    }
    let quote = flags.contains(ParameterFlags::LIST_QUOTE);
    Ok(arguments
        .iter()
        .map(|argument| match argument {
            SetArgument::Integer(value) => value.to_string(),
            SetArgument::Number(text) => text.clone(),
            SetArgument::Text(text) if quote => crate::expr::quote_ident(text),
            SetArgument::Text(text) => text.clone(),
        })
        .collect::<Vec<_>>()
        .join(", "))
}

#[cfg(test)]
mod tests;
