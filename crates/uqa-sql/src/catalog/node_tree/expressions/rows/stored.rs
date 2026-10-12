//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored constructors retain definition-time width and selected field conversions.

use super::{
    node_list, values, ColumnType, Expr, ExpressionContext, Field, Node, SQLError, TypedNode,
};
use crate::ast::CompositeRowBinding;
use crate::catalog::type_metadata::pg_type_oid;
use uqa_core::Value;

impl ExpressionContext<'_> {
    pub(in super::super) fn stored_row(
        &self,
        items: &[Expr],
        binding: &CompositeRowBinding,
    ) -> Result<TypedNode, SQLError> {
        let ty = self.resolve_type(&binding.ty)?;
        if !matches!(ty, ColumnType::Composite(_)) {
            return Err(super::super::super::invalid(
                "stored ROW type is not composite",
            ));
        }
        let layout = binding.layout.ok_or_else(|| {
            super::super::super::invalid("stored ROW has no definition-time layout")
        })?;
        let fields = self.stored_row_fields(items, binding)?;
        Ok(TypedNode {
            node: Node::new(
                "ROWEXPR",
                [
                    (
                        "args",
                        node_list(fields.into_iter().map(|value| value.node.into()).collect()),
                    ),
                    ("row_typeid", pg_type_oid(&ty).into()),
                    (
                        "row_format",
                        if layout.implicit { 2.into() } else { 1.into() },
                    ),
                    ("colnames", Field::Null),
                    ("location", (-1).into()),
                ],
            ),
            ty,
        })
    }

    pub(super) fn stored_row_fields(
        &self,
        items: &[Expr],
        binding: &CompositeRowBinding,
    ) -> Result<Vec<TypedNode>, SQLError> {
        let invalid = super::super::super::invalid;
        let layout = binding
            .layout
            .ok_or_else(|| invalid("stored ROW has no definition-time layout"))?;
        let types = binding
            .argument_types
            .as_ref()
            .ok_or_else(|| invalid("stored ROW has no admitted field types"))?;
        if layout.width < 0
            || binding.attributes.len() != items.len()
            || types.len() != items.len()
            || binding
                .attributes
                .iter()
                .any(|number| *number <= 0 || *number > layout.width)
            || binding.attributes.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(invalid("invalid stored ROW layout"));
        }
        let width =
            usize::try_from(layout.width).map_err(|_| invalid("invalid stored ROW layout"))?;
        let mut fields = Vec::with_capacity(width);
        let mut arguments = binding.attributes.iter().zip(items).zip(types).peekable();
        for position in 1..=layout.width {
            if arguments
                .peek()
                .is_none_or(|((number, _), _)| **number != position)
            {
                fields.push(TypedNode {
                    node: values::constant(&Value::Null, &ColumnType::Integer)?,
                    ty: ColumnType::Integer,
                });
                continue;
            }
            let ((_, expression), ty) = arguments.next().expect("matched stored ROW argument");
            let value = match expression {
                Expr::Cast {
                    expr,
                    ty,
                    implicit: true,
                } => self.row_field(expr, &self.resolve_type(ty)?)?,
                _ => self.encode(expression, None)?,
            };
            fields.push(Self::coerce(value, ty, 2)?);
        }
        Ok(fields)
    }
}
