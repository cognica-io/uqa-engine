//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coercion grouping follows the original typed child even when an implicit cast is hidden.

use super::{invalid, list, parentheses, Field, Node, Renderer, SQLError};

impl Renderer<'_> {
    pub(super) fn coercion(&self, node: &Node, outer: bool) -> Result<String, SQLError> {
        let argument = node.field("arg")?;
        let modifier = if node.kind == "COERCEVIAIO" {
            -1
        } else {
            node.integer("resulttypmod")?
        };
        Ok(parentheses(
            self.cast(argument, node.integer("resulttype")?, modifier)?,
            self.pretty && !outer && !simple_input(argument)?,
        ))
    }

    pub(super) fn cast(
        &self,
        argument: &Field,
        oid: i64,
        modifier: i64,
    ) -> Result<String, SQLError> {
        if let Field::Node(node) = argument {
            if node.kind == "CONST"
                && node.integer("consttype")? == oid
                && node.integer("consttypmod")? == -1
            {
                return Ok(format!(
                    "{}::{}",
                    self.constant_labeled(node, false)?,
                    self.names.type_name(oid, modifier)?
                ));
            }
        }
        Ok(format!(
            "{}::{}",
            parentheses(self.coercion_input(argument)?, !self.pretty),
            self.names.type_name(oid, modifier)?
        ))
    }

    fn coercion_input(&self, argument: &Field) -> Result<String, SQLError> {
        let value = match implicit_input(argument)? {
            Some(input) => self.coercion_input(input)?,
            None => self.field(argument, self.pretty)?,
        };
        Ok(parentheses(value, self.pretty && !simple_input(argument)?))
    }
}

fn simple_input(argument: &Field) -> Result<bool, SQLError> {
    let Field::Node(node) = argument else {
        return Err(invalid("cast operand is not an expression"));
    };
    Ok(match node.kind.as_str() {
        "RELABELTYPE" | "COERCEVIAIO" | "COERCETODOMAIN" | "ARRAYCOERCEEXPR" => {
            simple_input(node.field("arg")?)?
        }
        "VAR"
        | "PARAM"
        | "COERCETODOMAINVALUE"
        | "CONST"
        | "FUNCEXPR"
        | "NULLIFEXPR"
        | "COALESCEEXPR"
        | "MINMAXEXPR"
        | "ARRAYEXPR"
        | "CASEEXPR"
        | "ROWEXPR"
        | "SUBSCRIPTINGREF"
        | "FIELDSELECT" => true,
        _ => false,
    })
}

pub(super) fn implicit_cast_input(mut argument: &Field) -> Result<&Field, SQLError> {
    while matches!(argument, Field::Node(_)) {
        let Some(input) = implicit_input(argument)? else {
            break;
        };
        argument = input;
    }
    Ok(argument)
}

fn implicit_input(argument: &Field) -> Result<Option<&Field>, SQLError> {
    let Field::Node(node) = argument else {
        return Err(invalid("cast operand is not an expression"));
    };
    let format = match node.kind.as_str() {
        "FUNCEXPR" => "funcformat",
        "RELABELTYPE" => "relabelformat",
        "COERCEVIAIO" | "ARRAYCOERCEEXPR" => "coerceformat",
        "COERCETODOMAIN" => "coercionformat",
        _ => return Ok(None),
    };
    if node.integer(format)? != 2 {
        return Ok(None);
    }
    if node.kind == "FUNCEXPR" {
        list(node, "args")?
            .first()
            .map(Some)
            .ok_or_else(|| invalid("cast function has no argument"))
    } else {
        node.field("arg").map(Some)
    }
}
