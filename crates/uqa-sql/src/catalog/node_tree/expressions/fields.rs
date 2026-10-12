//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite projections retain physical attributes and the complete original argument.

use super::{Expr, ExpressionContext, Field, FunctionBinding, Node, SQLError, TypedNode};
use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_modifier, pg_type_oid};
use crate::type_resolution::field_selection;
use uqa_core::Value;

impl ExpressionContext<'_> {
    pub(super) fn field_select(
        &self,
        binding: &FunctionBinding,
        arguments: &[Expr],
    ) -> Result<TypedNode, SQLError> {
        let [base, Expr::Literal(Value::Str(field))] = arguments else {
            return Err(super::super::invalid("invalid field selection operands"));
        };
        let (input, number, ty, collation) = if let Some(field) = &binding.composite_field {
            let input = self.encode(base, None)?;
            let collation = field.collation_oid.unwrap_or_else(|| {
                u32::try_from(pg_type_collation_oid(&field.result_type))
                    .expect("field collation identity")
            });
            (
                input,
                i64::from(field.number),
                Some(field.result_type.clone()),
                collation,
            )
        } else if let Some(elements) = self.anonymous_row_elements(base)? {
            let position = field_selection::anonymous_field_position(field, elements.len())
                .ok_or_else(|| field_selection::undefined_record_field(field))?;
            let (input, ty) = self.row_with_selected_type(elements, position)?;
            let Field::List(items) = input.node.field("args")? else {
                return Err(super::super::invalid("invalid ROW field list"));
            };
            let Some(Field::Node(selected)) = items.get(position) else {
                return Err(super::super::invalid("invalid selected ROW field"));
            };
            let collation = super::collations::initial(selected)?;
            (
                input,
                i64::try_from(position + 1)
                    .map_err(|_| super::super::invalid("invalid field number"))?,
                ty,
                collation,
            )
        } else {
            let input = self.encode(base, None)?;
            let selected =
                field_selection::value_field_selection(input.ty.as_ref(), field, self.types)?
                    .ok_or_else(|| super::super::invalid("field descriptor is unavailable"))?;
            (
                input,
                i64::from(selected.number),
                Some(selected.ty),
                selected.collation,
            )
        };
        Ok(TypedNode {
            node: Node::new(
                "FIELDSELECT",
                [
                    ("arg", input.node.into()),
                    ("fieldnum", number.into()),
                    ("resulttype", ty.as_ref().map_or(705, pg_type_oid).into()),
                    (
                        "resulttypmod",
                        ty.as_ref().map_or(-1, pg_type_modifier).into(),
                    ),
                    ("resultcollid", i64::from(collation).into()),
                ],
            ),
            ty,
        })
    }
}
