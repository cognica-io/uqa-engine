//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct typed array endpoints, retaining omitted syntax and NULL constants.

use super::{implicit_cast_input, invalid, list, Field, Node, Renderer, SQLError};
use std::fmt::Write as _;

impl Renderer<'_> {
    pub(super) fn array_access(&self, node: &Node) -> Result<String, SQLError> {
        if node.field("refassgnexpr")? != &Field::Null {
            return Err(invalid("array read node contains an assignment"));
        }
        let upper = list(node, "refupperindexpr")?;
        let lower = list(node, "reflowerindexpr")?;
        let slice = !lower.is_empty();
        if upper.is_empty()
            || upper.len() > crate::ast::MAX_ARRAY_SUBSCRIPT_DIMENSIONS
            || (slice && lower.len() != upper.len())
        {
            return Err(invalid("invalid array bound dimensions"));
        }
        for name in ["refcontainertype", "refelemtype", "refrestype"] {
            if u32::try_from(node.integer(name)?)
                .ok()
                .is_none_or(|oid| oid == 0)
            {
                return Err(invalid("invalid array type identity"));
            }
        }
        let result = if slice {
            "refcontainertype"
        } else {
            "refelemtype"
        };
        if node.integer("refrestype")? != node.integer(result)? {
            return Err(invalid("inconsistent array result identity"));
        }
        let Field::Node(input) = node.field("refexpr")? else {
            return Err(invalid("array read operand is not an expression"));
        };
        let input_text = self.node(input, self.pretty)?;
        let mut text = if input.kind == "VAR" {
            input_text
        } else {
            format!("({input_text})")
        };
        for (position, upper) in upper.iter().enumerate() {
            text.push('[');
            if slice {
                write!(text, "{}:", self.array_bound(&lower[position], true)?)
                    .expect("writing a String");
            }
            write!(text, "{}]", self.array_bound(upper, slice)?).expect("writing a String");
        }
        Ok(text)
    }

    fn array_bound(&self, value: &Field, slice: bool) -> Result<String, SQLError> {
        if value == &Field::Null {
            if slice {
                return Ok(String::new());
            }
            return Err(invalid("array element index is absent"));
        }
        let Field::Node(bound) = implicit_cast_input(value)? else {
            return Err(invalid("array endpoint is not an expression"));
        };
        self.node(bound, self.pretty)
    }
}
