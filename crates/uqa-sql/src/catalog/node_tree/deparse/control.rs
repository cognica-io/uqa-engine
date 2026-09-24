//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CASE expression layout and its implicit comparison anchor.

use super::{invalid, list, Field, Node, Renderer, SQLError};
use std::fmt::Write;

impl Renderer<'_> {
    pub(super) fn case(&self, node: &Node) -> Result<String, SQLError> {
        let indent = " ".repeat(self.indent);
        let child = Self {
            indent: self.indent + 4,
            ..*self
        };
        let base = node.field("arg")?;
        let mut text = format!("\n{indent}CASE");
        if base != &Field::Null {
            write!(text, " {}", self.field(base, self.pretty)?).expect("writing a String");
        }
        for branch in list(node, "args")? {
            let Field::Node(branch) = branch else {
                return Err(invalid("expected CASEWHEN node"));
            };
            if branch.kind != "CASEWHEN" {
                return Err(invalid("expected CASEWHEN node"));
            }
            let mut condition = branch.field("expr")?;
            if base != &Field::Null {
                let Field::Node(comparison) = condition else {
                    return Err(invalid("invalid simple CASE comparison"));
                };
                let [_, right] = list(comparison, "args")? else {
                    return Err(invalid("invalid simple CASE operands"));
                };
                condition = right;
            }
            write!(
                text,
                "\n{indent}    WHEN {} THEN {}",
                child.field(condition, self.pretty)?,
                child.field(branch.field("result")?, self.pretty)?,
            )
            .expect("writing a String");
        }
        let otherwise = node.field("defresult")?;
        write!(
            text,
            "\n{indent}    ELSE {}\n{indent}END",
            child.field(otherwise, self.pretty)?
        )
        .expect("writing a String");
        Ok(text)
    }
}
