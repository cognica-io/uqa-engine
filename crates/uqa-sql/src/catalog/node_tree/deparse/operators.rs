//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Operator grouping and explicit or implicit function notation.

use super::{atom, invalid, list, parentheses, prefix, Field, Node, Renderer, SQLError};

impl Renderer<'_> {
    pub(super) fn operator(&self, node: &Node, outer: bool) -> Result<String, SQLError> {
        if let [argument] = list(node, "args")? {
            let operator = crate::type_resolution::unary_operator_by_oid(node.integer("opno")?)
                .ok_or_else(|| invalid("unknown unary operator"))?;
            return Ok(parentheses(
                format!("{} {}", operator.name, self.field(argument, false)?),
                !outer,
            ));
        }
        let operator = crate::type_resolution::binary_operator_by_oid(node.integer("opno")?)
            .ok_or_else(|| invalid("unknown catalog expression operator"))?;
        let [left, right] = list(node, "args")? else {
            return Err(invalid("invalid binary operator operands"));
        };
        Ok(parentheses(
            format!(
                "{} {} {}",
                self.field(left, false)?,
                if node.kind == "DISTINCTEXPR" {
                    "IS DISTINCT FROM"
                } else {
                    operator.name
                },
                self.field(right, false)?
            ),
            !outer,
        ))
    }

    pub(super) fn function(&self, node: &Node) -> Result<String, SQLError> {
        let args = list(node, "args")?;
        let format = node.integer("funcformat")?;
        if matches!(format, 1 | 2) {
            let arg = args
                .first()
                .ok_or_else(|| invalid("cast function has no argument"))?;
            let modifier = match args.get(1) {
                Some(Field::Node(constant))
                    if constant.kind == "CONST" && constant.integer("consttype")? == 23 =>
                {
                    let Field::Datum { bytes, .. } = constant.field("constvalue")? else {
                        return Err(invalid("cast modifier has no Datum"));
                    };
                    i64::from(i32::from_le_bytes(prefix(bytes)?))
                }
                _ => -1,
            };
            return self.cast(arg, node.integer("funcresulttype")?, modifier);
        }
        if format == 3 {
            return Err(SQLError::Unsupported(format!(
                "catalog function syntax format {format}"
            )));
        }
        if format != 0 {
            return Err(invalid("invalid function coercion format"));
        }
        let name = self
            .names
            .routine(node.integer("funcid")?)?
            .iter()
            .map(|part| crate::expr::quote_ident(part))
            .collect::<Vec<_>>()
            .join(".");
        let arguments = args
            .iter()
            .map(|arg| self.field(arg, false))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(format!("{name}({})", arguments.join(", ")))
    }

    pub(super) fn boolean(&self, node: &Node, outer: bool) -> Result<String, SQLError> {
        let args = list(node, "args")?;
        let operator = atom(node, "boolop")?;
        let value = match operator {
            "not" if args.len() == 1 => {
                let grouping = matches!(&args[0], Field::Node(node) if node.kind == "BOOLEXPR");
                format!("NOT {}", self.field(&args[0], self.pretty && !grouping)?)
            }
            "and" | "or" => {
                let join = if operator == "and" { " AND " } else { " OR " };
                args.iter()
                    .map(|arg| {
                        let grouping = matches!(arg, Field::Node(node)
                                    if node.kind == "BOOLEXPR"
                                        && operator == "and"
                                        && atom(node, "boolop").is_ok_and(|child| child == "or"));
                        self.field(arg, self.pretty && !grouping)
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join(join)
            }
            _ => return Err(invalid("invalid boolean expression operands")),
        };
        Ok(parentheses(value, !outer))
    }
}
