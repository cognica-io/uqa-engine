//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named ROW coercion uses declared attribute positions and the selected field casts.

use super::{
    node_list, values, ColumnType, Expr, ExpressionContext, Field, Node, SQLError, TypedNode,
};
use crate::catalog::type_metadata::pg_type_oid;
use crate::expr::composites;
use crate::FunctionTypeResolver;
use uqa_core::Value;

impl ExpressionContext<'_> {
    pub(in super::super) fn row_cast(
        &self,
        expression: &Expr,
        target: &ColumnType,
        format: i64,
    ) -> Result<Option<TypedNode>, SQLError> {
        let Some(elements) = self.anonymous_row_elements(expression)? else {
            return Ok(None);
        };
        let mut base = target;
        while let ColumnType::Domain { base: inner, .. } = base {
            base = inner;
        }
        let ColumnType::Composite(reference) = base else {
            return Ok(None);
        };
        let fields = self.named_row_fields(elements, target, reference.oid)?;
        let row = TypedNode {
            node: Node::new(
                "ROWEXPR",
                [
                    (
                        "args",
                        node_list(fields.into_iter().map(|value| value.node.into()).collect()),
                    ),
                    ("row_typeid", pg_type_oid(base).into()),
                    (
                        "row_format",
                        if target == base {
                            format.into()
                        } else {
                            2.into()
                        },
                    ),
                    ("colnames", Field::Null),
                    ("location", (-1).into()),
                ],
            ),
            ty: base.clone(),
        };
        if target == base {
            Ok(Some(row))
        } else {
            Self::coerce(row, target, format).map(Some)
        }
    }

    fn anonymous_row_elements<'a>(
        &self,
        expression: &'a Expr,
    ) -> Result<Option<&'a [Expr]>, SQLError> {
        match expression {
            Expr::Row(elements) => Ok(Some(elements)),
            Expr::Cast { expr, ty, .. } if self.resolve_type(ty)? == ColumnType::Record => {
                self.anonymous_row_elements(expr)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn named_row_fields(
        &self,
        elements: &[Expr],
        target: &ColumnType,
        oid: u32,
    ) -> Result<Vec<TypedNode>, SQLError> {
        let descriptor = composites::descriptor(
            self.types.and_then(FunctionTypeResolver::composite_types),
            oid,
        )?;
        crate::type_resolution::composite_rows::validate_width(
            target,
            elements.len(),
            descriptor.attributes.len(),
        )?;
        let mut fields = Vec::new();
        let mut arguments = elements.iter();
        let width = descriptor
            .attributes
            .iter()
            .map(|attribute| attribute.number)
            .chain(descriptor.dropped.iter().map(|attribute| attribute.number))
            .max()
            .unwrap_or(0);
        for position in 1..=width {
            let Some(attribute) = descriptor
                .attributes
                .iter()
                .find(|attribute| attribute.number == position)
            else {
                fields.push(TypedNode {
                    node: values::constant(&Value::Null, &ColumnType::Integer)?,
                    ty: ColumnType::Integer,
                });
                continue;
            };
            let expression = arguments.next().expect("validated ROW width");
            fields.push(self.row_field(expression, &attribute.ty)?);
        }
        Ok(fields)
    }

    fn row_field(&self, expression: &Expr, target: &ColumnType) -> Result<TypedNode, SQLError> {
        if let Some(row) = self.row_cast(expression, target, 2)? {
            return Ok(row);
        }
        let mut input = target;
        while let ColumnType::Domain { base, .. } = input {
            input = base;
        }
        let input = input.without_type_modifiers();
        let unknown = self.expression_type(expression)?.is_none();
        let value = self.encode(expression, unknown.then_some(&input))?;
        Self::coerce_row_field(value, target)
    }
}
