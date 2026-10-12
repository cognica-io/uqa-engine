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

use super::inputs::{anonymous_row, RowInput};

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
        Self::named_row(fields, base, target, format).map(Some)
    }

    fn named_row(
        fields: Vec<TypedNode>,
        base: &ColumnType,
        target: &ColumnType,
        format: i64,
    ) -> Result<TypedNode, SQLError> {
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
            Ok(row)
        } else {
            Self::coerce(row, target, format)
        }
    }

    pub(super) fn anonymous_row_elements<'a>(
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
        self.coerce_row_inputs(self.row_inputs(elements)?, target, oid)
    }

    fn coerce_row_inputs(
        &self,
        inputs: Vec<RowInput<'_>>,
        target: &ColumnType,
        oid: u32,
    ) -> Result<Vec<TypedNode>, SQLError> {
        let descriptor = composites::descriptor(
            self.types.and_then(FunctionTypeResolver::composite_types),
            oid,
        )?;
        let mut fields = Vec::new();
        let input_width = inputs.len();
        let mut arguments = inputs.into_iter().enumerate();
        let width = descriptor.physical_width();
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
            let Some((index, input)) = arguments.next() else {
                return Err(crate::type_resolution::composite_rows::row_cast_error(
                    target,
                    "Input has too few columns.".into(),
                    self.types,
                ));
            };
            crate::type_resolution::composite_rows::validate_field_cast(
                target,
                input.ty(),
                &attribute.ty,
                index + 1,
                self.types,
            )?;
            fields.push(self.coerce_row_input(input, &attribute.ty)?);
        }
        crate::type_resolution::composite_rows::validate_width(
            target,
            input_width,
            descriptor.attributes.len(),
            self.types,
        )?;
        Ok(fields)
    }

    pub(super) fn row_field(
        &self,
        expression: &Expr,
        target: &ColumnType,
    ) -> Result<TypedNode, SQLError> {
        self.coerce_row_input(self.row_input(expression)?, target)
    }

    fn coerce_row_input(
        &self,
        input: RowInput<'_>,
        target: &ColumnType,
    ) -> Result<TypedNode, SQLError> {
        let mut base = target;
        while let ColumnType::Domain { base: nested, .. } = base {
            base = nested;
        }
        let value = match input {
            RowInput::Collated { mut argument, oid } => {
                while let RowInput::Collated {
                    argument: inner, ..
                } = *argument
                {
                    argument = inner;
                }
                let mut value = self.coerce_row_input(*argument, target)?;
                if crate::schema::composites::type_is_collatable(target) {
                    value.node = super::super::collations::collate(value.node, i64::from(oid));
                }
                return Ok(value);
            }
            RowInput::Unknown(expression) => {
                self.encode(expression, Some(&base.without_type_modifiers()))?
            }
            RowInput::Row(inputs) => {
                if let ColumnType::Composite(reference) = base {
                    let fields = self.coerce_row_inputs(inputs, target, reference.oid)?;
                    return Self::named_row(fields, base, target, 2);
                }
                anonymous_row(inputs)?
            }
            RowInput::Value(value) => value,
        };
        Self::coerce_row_field(value, target)
    }
}
