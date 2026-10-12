//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve live composite field names by their retained physical attribute numbers.

use super::{invalid, list, Field, Node, Renderer, SQLError};

impl Renderer<'_> {
    pub(super) fn field_select(&self, node: &Node) -> Result<String, SQLError> {
        let number = i16::try_from(node.integer("fieldnum")?)
            .ok()
            .filter(|number| *number > 0)
            .ok_or_else(|| invalid("invalid composite field number"))?;
        if u32::try_from(node.integer("resulttype")?)
            .ok()
            .is_none_or(|oid| oid == 0)
            || u32::try_from(node.integer("resultcollid")?).is_err()
        {
            return Err(invalid("invalid composite field result identity"));
        }
        let Field::Node(input) = node.field("arg")? else {
            return Err(invalid("field selection operand is not an expression"));
        };
        let oid = super::types::result_type(input)?;
        let name = if oid == 2249 && input.kind == "ROWEXPR" {
            let position = usize::try_from(number - 1).expect("positive field number");
            let args = list(input, "args")?;
            let names = list(input, "colnames")?;
            if args.len() != names.len() || position >= args.len() {
                return Err(invalid("invalid ROW field descriptor"));
            }
            let Field::StringValue(name) = &names[position] else {
                return Err(invalid("invalid ROW field name"));
            };
            name.clone()
        } else {
            let catalog = self
                .names
                .value_catalog()
                .ok_or_else(|| invalid("catalog type metadata is unavailable"))?;
            let ty = catalog
                .value_type_by_oid(oid)?
                .ok_or_else(|| invalid("unknown composite field input type"))?;
            let mut base = &ty;
            while let crate::ColumnType::Domain { base: inner, .. } = base {
                base = inner;
            }
            let crate::ColumnType::Composite(reference) = base else {
                return Err(invalid("field selection input is not a composite type"));
            };
            let descriptor = catalog
                .value_composite_type(reference.oid)?
                .ok_or_else(|| invalid("composite field descriptor is unavailable"))?;
            descriptor
                .attributes
                .iter()
                .find(|attribute| attribute.number == number)
                .ok_or_else(|| invalid("composite field attribute is unavailable"))?
                .name
                .clone()
        };
        let base = self.node(input, self.pretty)?;
        let base = if matches!(input.kind.as_str(), "FIELDSELECT" | "SUBSCRIPTINGREF") {
            base
        } else {
            format!("({base})")
        };
        Ok(format!("{base}.{}", crate::expr::quote_ident(&name)))
    }
}
