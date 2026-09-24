//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` node-tree text used by stored catalog expressions.

use std::fmt;

use crate::SQLError;

mod read;
pub use read::parse;
pub mod deparse;
pub mod expressions;
mod values;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub kind: String,
    pub fields: Vec<(String, Field)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    Null,
    Atom(String),
    String(String),
    Node(Node),
    List(Vec<Field>),
    /// `PostgreSQL` writes by-value Datums at machine-word width even when the declared type is narrower.
    Datum {
        length: usize,
        bytes: Vec<u8>,
    },
}

impl Node {
    pub fn new(
        kind: impl Into<String>,
        fields: impl IntoIterator<Item = (&'static str, Field)>,
    ) -> Self {
        Self {
            kind: kind.into(),
            fields: fields
                .into_iter()
                .map(|(name, value)| (name.into(), value))
                .collect(),
        }
    }

    pub fn field(&self, name: &str) -> Result<&Field, SQLError> {
        self.fields
            .iter()
            .find_map(|(field, value)| (field == name).then_some(value))
            .ok_or_else(|| invalid(format!("missing {name} in {} node", self.kind)))
    }

    pub fn integer(&self, name: &str) -> Result<i64, SQLError> {
        match self.field(name)? {
            Field::Atom(value) => value
                .parse()
                .map_err(|_| invalid(format!("invalid integer field {name}"))),
            _ => Err(invalid(format!("invalid integer field {name}"))),
        }
    }

    pub fn boolean(&self, name: &str) -> Result<bool, SQLError> {
        match self.field(name)? {
            Field::Atom(value) if value == "true" => Ok(true),
            Field::Atom(value) if value == "false" => Ok(false),
            _ => Err(invalid(format!("invalid boolean field {name}"))),
        }
    }
}

impl From<i64> for Field {
    fn from(value: i64) -> Self {
        Self::Atom(value.to_string())
    }
}

impl From<bool> for Field {
    fn from(value: bool) -> Self {
        Self::Atom(value.to_string())
    }
}

impl From<Node> for Field {
    fn from(value: Node) -> Self {
        Self::Node(value)
    }
}

impl fmt::Display for Node {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "{{{}", self.kind)?;
        for (name, value) in &self.fields {
            write!(output, " :{name} {value}")?;
        }
        output.write_str("}")
    }
}

impl fmt::Display for Field {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => output.write_str("<>"),
            Self::Atom(value) => output.write_str(value),
            Self::String(value) => write_token(output, value),
            Self::Node(node) => node.fmt(output),
            Self::List(values) => {
                output.write_str("(")?;
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        output.write_str(" ")?;
                    }
                    value.fmt(output)?;
                }
                output.write_str(")")
            }
            Self::Datum { length, bytes } => {
                write!(output, "{length} [")?;
                for byte in bytes {
                    // PostgreSQL promotes native C char, whose signedness is platform-specific.
                    write!(output, " {}", std::ffi::c_char::from_ne_bytes([*byte]))?;
                }
                output.write_str(" ]")
            }
        }
    }
}

fn write_token(output: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    if value.is_empty() {
        return output.write_str("\"\"");
    }
    let mut bytes = value.bytes();
    let first = bytes.next().expect("nonempty token");
    if matches!(first, b'<' | b'"')
        || first.is_ascii_digit()
        || (matches!(first, b'+' | b'-')
            && bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_digit() || byte == b'.'))
    {
        output.write_str("\\")?;
    }
    for character in value.chars() {
        if matches!(character, ' ' | '\n' | '\t' | '(' | ')' | '{' | '}' | '\\') {
            output.write_str("\\")?;
        }
        write!(output, "{character}")?;
    }
    Ok(())
}

pub(super) fn invalid(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX000".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
