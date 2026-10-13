//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Operator grouping and explicit or implicit function notation.

use super::{atom, invalid, list, parentheses, prefix, values, Field, Node, Renderer, SQLError};

impl Renderer<'_> {
    pub(super) fn boolean_test(&self, node: &Node, outer: bool) -> Result<String, SQLError> {
        let test = crate::ast::BooleanTest::from_catalog_code(node.integer("booltesttype")?)
            .ok_or_else(|| invalid("invalid Boolean test"))?;
        Ok(parentheses(
            format!(
                "{} {}",
                self.field(node.field("arg")?, false)?,
                test.label()
            ),
            !outer,
        ))
    }

    pub(super) fn null_test(&self, node: &Node, outer: bool) -> Result<String, SQLError> {
        let scalar_row_test =
            !node.boolean("argisrow")? && self.is_record_argument(node.field("arg")?)?;
        let operator = match (node.integer("nulltesttype")?, scalar_row_test) {
            (0, false) => "IS NULL",
            (1, false) => "IS NOT NULL",
            (0, true) => "IS NOT DISTINCT FROM NULL",
            (1, true) => "IS DISTINCT FROM NULL",
            _ => return Err(invalid("invalid null test")),
        };
        Ok(parentheses(
            format!("{} {operator}", self.field(node.field("arg")?, false)?),
            !outer,
        ))
    }

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
                self.arithmetic_operand(left, operator.name, true)?,
                if node.kind == "DISTINCTEXPR" {
                    "IS DISTINCT FROM"
                } else {
                    operator.name
                },
                self.arithmetic_operand(right, operator.name, false)?
            ),
            !outer,
        ))
    }

    fn arithmetic_operand(
        &self,
        argument: &Field,
        parent: &str,
        left: bool,
    ) -> Result<String, SQLError> {
        // PostgreSQL's isSimpleNode removes parentheses only for basic binary arithmetic, retaining a right operand of the same priority.
        let simple = match (arithmetic_priority(parent), argument) {
            (Some(parent), Field::Node(node))
                if node.kind == "OPEXPR" && list(node, "args")?.len() == 2 =>
            {
                crate::type_resolution::binary_operator_by_oid(node.integer("opno")?)
                    .and_then(|operator| arithmetic_priority(operator.name))
                    .is_some_and(|child| child > parent || (left && child == parent))
            }
            _ => false,
        };
        self.field(argument, self.pretty && simple)
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
            if (6199..=6204).contains(&node.integer("funcid")?) {
                return self.extract(args);
            }
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
        let mut arguments = args
            .iter()
            .map(|arg| self.field(arg, self.pretty))
            .collect::<Result<Vec<_>, _>>()?;
        if node.boolean("funcvariadic")? {
            let last = arguments
                .last_mut()
                .ok_or_else(|| invalid("variadic call has no argument"))?;
            last.insert_str(0, "VARIADIC ");
        }
        Ok(format!("{name}({})", arguments.join(", ")))
    }

    fn extract(&self, arguments: &[Field]) -> Result<String, SQLError> {
        let [Field::Node(field), source] = arguments else {
            return Err(invalid("invalid EXTRACT operands"));
        };
        if field.kind != "CONST"
            || field.integer("consttype")? != 25
            || field.boolean("constisnull")?
            || field.boolean("constbyval")?
        {
            return Err(invalid("EXTRACT field is not a text constant"));
        }
        let Field::Datum { length, bytes } = field.field("constvalue")? else {
            return Err(invalid("EXTRACT field has no Datum"));
        };
        let field = std::str::from_utf8(values::varlena_payload(*length, bytes)?)
            .map_err(|_| invalid("invalid UTF-8 in EXTRACT field"))?;
        Ok(format!(
            "EXTRACT({field} FROM {})",
            self.field(source, false)?
        ))
    }

    pub(super) fn boolean(&self, node: &Node, outer: bool) -> Result<String, SQLError> {
        let args = list(node, "args")?;
        let operator = atom(node, "boolop")?;
        let value = match operator {
            "not" if args.len() == 1 => {
                let grouping = matches!(&args[0], Field::Node(node) if node.kind == "BOOLEXPR");
                let row_comparison = self.pretty
                    && matches!(&args[0], Field::Node(node) if node.kind == "ROWCOMPAREEXPR");
                format!(
                    "NOT {}",
                    parentheses(
                        self.field(&args[0], self.pretty && !grouping)?,
                        row_comparison
                    )
                )
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

fn arithmetic_priority(operator: &str) -> Option<u8> {
    match operator {
        "+" | "-" => Some(1),
        "*" | "/" | "%" => Some(2),
        _ => None,
    }
}
