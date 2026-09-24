//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct SQL from typed catalog nodes using the caller's catalog names.

use super::{invalid, values, Field, Node};
use crate::SQLError;

mod control;
mod operators;

pub trait ExpressionNames {
    fn column(&self, attribute: i64) -> Result<String, SQLError>;
    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError>;
    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError>;
}

pub fn expression(
    value: &Field,
    names: &dyn ExpressionNames,
    pretty: bool,
) -> Result<String, SQLError> {
    Renderer {
        names,
        pretty,
        indent: 0,
    }
    .field(value, pretty)
}

struct Renderer<'a> {
    names: &'a dyn ExpressionNames,
    pretty: bool,
    indent: usize,
}

impl Renderer<'_> {
    fn field(&self, value: &Field, outer: bool) -> Result<String, SQLError> {
        match value {
            Field::Node(node) => self.node(node, outer),
            Field::List(values) => values
                .iter()
                .map(|value| self.field(value, outer))
                .collect::<Result<Vec<_>, _>>()
                .map(|values| values.join(", ")),
            _ => Err(invalid("expected an expression node")),
        }
    }

    fn node(&self, node: &Node, outer: bool) -> Result<String, SQLError> {
        match node.kind.as_str() {
            "COERCETODOMAINVALUE" => Ok("VALUE".into()),
            "VAR" => {
                if node.integer("varno")? != 1 || node.integer("varlevelsup")? != 0 {
                    return Err(invalid("invalid varno in stored expression"));
                }
                self.names
                    .column(node.integer("varattno")?)
                    .map(|name| crate::expr::quote_ident(&name))
            }
            "CONST" => self.constant(node),
            "OPEXPR" | "DISTINCTEXPR" => self.operator(node, outer),
            "FUNCEXPR" => self.function(node),
            "RELABELTYPE" => self.cast(
                node.field("arg")?,
                node.integer("resulttype")?,
                node.integer("resulttypmod")?,
            ),
            "COERCEVIAIO" => self.cast(node.field("arg")?, node.integer("resulttype")?, -1),
            "COERCETODOMAIN" => self.cast(
                node.field("arg")?,
                node.integer("resulttype")?,
                node.integer("resulttypmod")?,
            ),
            "BOOLEXPR" => self.boolean(node, outer),
            "NULLTEST" => {
                let operator = match node.integer("nulltesttype")? {
                    0 => "IS NULL",
                    1 => "IS NOT NULL",
                    _ => return Err(invalid("invalid null test")),
                };
                Ok(parentheses(
                    format!("{} {operator}", self.field(node.field("arg")?, false)?),
                    !outer,
                ))
            }
            "COALESCEEXPR" | "MINMAXEXPR" => {
                let name = match node.kind.as_str() {
                    "COALESCEEXPR" => "COALESCE",
                    _ => match node.integer("op")? {
                        0 => "GREATEST",
                        1 => "LEAST",
                        _ => return Err(invalid("invalid minimum/maximum expression")),
                    },
                };
                let arguments = list(node, "args")?
                    .iter()
                    .map(|arg| self.field(arg, false))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("{name}({})", arguments.join(", ")))
            }
            "ARRAYEXPR" => {
                let elements = list(node, "elements")?
                    .iter()
                    .map(|arg| self.field(arg, self.pretty))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("ARRAY[{}]", elements.join(", ")))
            }
            "CASEEXPR" => self.case(node),
            "SCALARARRAYOPEXPR" => {
                let operator =
                    crate::type_resolution::binary_operator_by_oid(node.integer("opno")?)
                        .ok_or_else(|| invalid("unknown scalar-array operator"))?;
                let [left, right] = list(node, "args")? else {
                    return Err(invalid("invalid scalar-array operands"));
                };
                let quantifier = if node.boolean("useOr")? { "ANY" } else { "ALL" };
                Ok(parentheses(
                    format!(
                        "{} {} {quantifier} ({})",
                        self.field(left, false)?,
                        operator.name,
                        self.field(right, false)?
                    ),
                    !outer,
                ))
            }
            _ => Err(SQLError::Unsupported(format!(
                "catalog expression deparser for {}",
                node.kind
            ))),
        }
    }

    fn cast(&self, argument: &Field, oid: i64, modifier: i64) -> Result<String, SQLError> {
        let text = self.field(argument, false)?;
        let atomic = matches!(argument, Field::Node(node) if matches!(node.kind.as_str(), "VAR" | "COERCETODOMAINVALUE" | "CONST" | "FUNCEXPR"));
        Ok(format!(
            "{}::{}",
            parentheses(text, !self.pretty || !atomic),
            self.names.type_name(oid, modifier)?
        ))
    }

    fn constant(&self, node: &Node) -> Result<String, SQLError> {
        let oid = node.integer("consttype")?;
        let modifier = node.integer("consttypmod")?;
        let type_name = || self.names.type_name(oid, modifier);
        if node.boolean("constisnull")? {
            return Ok(format!("NULL::{}", type_name()?));
        }
        let Field::Datum { length, bytes } = node.field("constvalue")? else {
            return Err(invalid("constant has no Datum"));
        };
        if node.boolean("constbyval")? && (bytes.len() != 8 || !matches!(*length, 1 | 2 | 4 | 8)) {
            return Err(invalid("invalid by-value Datum length"));
        }
        let quoted = |value: &str| -> Result<String, SQLError> {
            Ok(format!("{}::{}", literal(value), type_name()?))
        };
        match oid {
            16 => match bytes.first() {
                Some(0) => Ok("false".into()),
                Some(1) => Ok("true".into()),
                _ => Err(invalid("invalid boolean Datum")),
            },
            20 | 21 | 23 | 26 | 28 => {
                let value = match oid {
                    21 => i64::from(i16::from_le_bytes(prefix(bytes)?)),
                    23 => i64::from(i32::from_le_bytes(prefix(bytes)?)),
                    26 | 28 => i64::from(u32::from_le_bytes(prefix(bytes)?)),
                    _ => i64::from_le_bytes(prefix(bytes)?),
                };
                if oid == 23 && value >= 0 {
                    Ok(value.to_string())
                } else {
                    quoted(&value.to_string())
                }
            }
            700 | 701 => {
                let value = if oid == 700 {
                    f64::from(f32::from_le_bytes(prefix(bytes)?))
                } else {
                    f64::from_le_bytes(prefix(bytes)?)
                };
                quoted(&uqa_core::format_float_pg(value))
            }
            18 => quoted(
                std::str::from_utf8(
                    bytes
                        .get(..1)
                        .ok_or_else(|| invalid("empty character Datum"))?,
                )
                .map_err(|_| invalid("invalid character Datum"))?,
            ),
            19 => {
                let end = bytes
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(bytes.len());
                quoted(
                    std::str::from_utf8(&bytes[..end])
                        .map_err(|_| invalid("invalid name Datum"))?,
                )
            }
            25 | 1042 | 1043 | 1790 => quoted(
                std::str::from_utf8(values::varlena_payload(*length, bytes)?)
                    .map_err(|_| invalid("invalid string Datum"))?,
            ),
            1700 => {
                let text = values::numeric::decode(values::varlena_payload(*length, bytes)?)?;
                if text.as_bytes().first().is_some_and(u8::is_ascii_digit) && text.contains('.') {
                    if modifier < 0 {
                        Ok(text)
                    } else {
                        Ok(format!("{text}::{}", type_name()?))
                    }
                } else {
                    quoted(&text)
                }
            }
            1082 | 1083 | 1114 | 1184 | 1186 | 1266 => {
                quoted(&values::temporal::decode(bytes, oid)?.to_sql_string())
            }
            _ => Err(SQLError::Unsupported(format!(
                "catalog expression Datum deparser for type {oid}"
            ))),
        }
    }
}

fn list<'a>(node: &'a Node, name: &str) -> Result<&'a [Field], SQLError> {
    match node.field(name)? {
        Field::List(values) => Ok(values),
        Field::Null => Ok(&[]),
        _ => Err(invalid(format!("expected node list in {name}"))),
    }
}

fn atom<'a>(node: &'a Node, name: &str) -> Result<&'a str, SQLError> {
    match node.field(name)? {
        Field::Atom(value) | Field::String(value) => Ok(value),
        _ => Err(invalid(format!("expected token in {name}"))),
    }
}

fn prefix<const N: usize>(bytes: &[u8]) -> Result<[u8; N], SQLError> {
    bytes
        .get(..N)
        .and_then(|prefix| prefix.try_into().ok())
        .ok_or_else(|| invalid("truncated constant Datum"))
}

fn parentheses(text: String, required: bool) -> String {
    if required {
        format!("({text})")
    } else {
        text
    }
}

fn literal(value: &str) -> String {
    let value = value.replace('\'', "''");
    if value.contains('\\') {
        format!("E'{}'", value.replace('\\', "\\\\"))
    } else {
        format!("'{value}'")
    }
}
