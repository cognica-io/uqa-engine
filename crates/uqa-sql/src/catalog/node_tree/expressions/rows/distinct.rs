//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DISTINCT expands transformed ROW constructors while preserving selected field coercions.

use super::{
    boolean, boolean_node, require_equal_width, values, BinaryOp, ColumnType, Expr,
    ExpressionContext, SQLError, TypedNode,
};
use crate::ast::CompositeRowBinding;
use crate::type_resolution::{binary_operator_catalog_entry, binary_operator_types};
use uqa_core::Value;

enum Constructor<'a> {
    Anonymous(&'a [Expr]),
    Named(&'a [Expr], ColumnType),
    Stored(&'a [Expr], &'a CompositeRowBinding),
}

enum Argument<'a> {
    Expression(&'a Expr),
    Selected(TypedNode),
}

impl ExpressionContext<'_> {
    pub(in super::super) fn row_distinct(
        &self,
        left: &Expr,
        right: &Expr,
    ) -> Result<Option<TypedNode>, SQLError> {
        let (Some(left), Some(right)) = (self.row_constructor(left)?, self.row_constructor(right)?)
        else {
            return Ok(None);
        };
        let left = self.constructor_arguments(left)?;
        let right = self.constructor_arguments(right)?;
        require_equal_width(&left, &right)?;
        let mut result = None;
        for (left, right) in left.into_iter().zip(right) {
            let [left_type, right_type, result_type] = binary_operator_types(
                BinaryOp::Equal,
                self.argument_type(&left)?.as_ref(),
                self.argument_type(&right)?.as_ref(),
            )?;
            let identity =
                binary_operator_catalog_entry(BinaryOp::Equal, [&left_type, &right_type])?;
            let mut selected = super::super::operator_node(
                identity.oid,
                identity.function_oid,
                [
                    self.select_argument(left, &left_type)?,
                    self.select_argument(right, &right_type)?,
                ],
                result_type,
            )
            .node;
            selected.kind = "DISTINCTEXPR".into();
            result = Some(match result {
                None => selected,
                Some(previous) => boolean("or", vec![previous, selected]),
            });
        }
        Ok(Some(boolean_node(match result {
            Some(node) => node,
            None => values::constant(&Value::Bool(false), &ColumnType::Boolean)?,
        })))
    }

    fn row_constructor<'a>(
        &self,
        expression: &'a Expr,
    ) -> Result<Option<Constructor<'a>>, SQLError> {
        match expression {
            Expr::Row(elements) => Ok(Some(Constructor::Anonymous(elements))),
            Expr::CompositeRow { items, binding } => Ok(Some(Constructor::Stored(items, binding))),
            Expr::Cast { expr, ty, .. } => {
                let target = self.resolve_type(ty)?;
                match &target {
                    ColumnType::Record => self.row_constructor(expr),
                    ColumnType::Composite(_) => match self.row_constructor(expr)? {
                        Some(Constructor::Anonymous(elements)) => {
                            Ok(Some(Constructor::Named(elements, target)))
                        }
                        Some(Constructor::Named(elements, source)) if source == target => {
                            Ok(Some(Constructor::Named(elements, source)))
                        }
                        Some(Constructor::Stored(items, binding))
                            if self.resolve_type(&binding.ty)? == target =>
                        {
                            Ok(Some(Constructor::Stored(items, binding)))
                        }
                        _ => Ok(None),
                    },
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    fn constructor_arguments<'a>(
        &self,
        constructor: Constructor<'a>,
    ) -> Result<Vec<Argument<'a>>, SQLError> {
        match constructor {
            Constructor::Anonymous(elements) => {
                Ok(elements.iter().map(Argument::Expression).collect())
            }
            Constructor::Named(elements, target) => {
                let ColumnType::Composite(reference) = &target else {
                    unreachable!("named constructor has a composite target")
                };
                Ok(self
                    .named_row_fields(elements, &target, reference.oid)?
                    .into_iter()
                    .map(Argument::Selected)
                    .collect())
            }
            Constructor::Stored(items, binding) => Ok(self
                .stored_row_fields(items, binding)?
                .into_iter()
                .map(Argument::Selected)
                .collect()),
        }
    }

    fn argument_type(&self, argument: &Argument<'_>) -> Result<Option<ColumnType>, SQLError> {
        match argument {
            Argument::Expression(expression) => self.expression_type(expression),
            Argument::Selected(value) => Ok(Some(value.ty.clone())),
        }
    }

    fn select_argument(
        &self,
        argument: Argument<'_>,
        target: &ColumnType,
    ) -> Result<TypedNode, SQLError> {
        match argument {
            Argument::Expression(expression) => self.encode(expression, Some(target)),
            Argument::Selected(value) => Self::coerce(value, target, 2),
        }
    }
}
