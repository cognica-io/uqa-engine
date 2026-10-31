//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type binding retains independent mutable scalar owners through every structural rewrite.

use super::{context::BinderContext, LiteralBinding};
use crate::{
    plan::OwnedScalarExpr, schema::ScalarTypeSchema, ColumnType, FunctionTypeResolver, SQLError,
    SQLParam, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

mod calls;
mod coercion;
mod membership;

#[cfg(test)]
mod tests;

pub fn bind_type_introspection_owned(
    expression: OwnedScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    OwnedBinder {
        context: BinderContext {
            schema,
            params,
            resolver,
            control: *control,
            literal_binding: LiteralBinding::Ordinary,
        },
    }
    .bind(expression)
}

pub(crate) fn bind_stored_inputs_owned(
    expression: OwnedScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    OwnedBinder {
        context: BinderContext {
            schema,
            params,
            resolver: Some(resolver),
            control: *control,
            literal_binding: LiteralBinding::Stored,
        },
    }
    .bind(expression)
}

/// Validate catalog literal coercions through the same owning binder used by execution planning. The temporary rewritten tree releases every constructor before returning.
pub fn validate_catalog_literals_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if !resolver
        .enum_labels()
        .is_some_and(crate::expr::enums::EnumLabelCatalog::has_enum_types)
    {
        return Ok(());
    }
    OwnedBinder {
        context: BinderContext {
            schema,
            params,
            resolver: Some(resolver),
            control: *control,
            literal_binding: LiteralBinding::Validate,
        },
    }
    .bind(expression.copy_for_rewrite(control)?)
    .map(drop)
}

struct OwnedBinder<'a> {
    context: BinderContext<'a>,
}

impl<'a> std::ops::Deref for OwnedBinder<'a> {
    type Target = BinderContext<'a>;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl OwnedBinder<'_> {
    fn bind(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        expression.rewrite(&self.context.control, |expression| {
            self.bind_node(expression)
        })
    }

    fn bind_node(&self, mut expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        match &*expression {
            ScalarExpr::Func { .. } => self.bind_call(expression),
            ScalarExpr::Array(_) => {
                expression = expression.map_children(control, |_, child| self.bind(child))?;
                let ScalarExpr::Array(items) = &*expression else {
                    unreachable!()
                };
                let target = self.context.common_type(items.iter())?;
                if let Some(target) = target {
                    expression = expression
                        .map_children(control, |_, child| self.common_cast(child, &target))?;
                }
                Ok(expression)
            }
            ScalarExpr::CompositeRow { .. } => self.bind_composite(expression),
            ScalarExpr::Binary { .. } => self.bind_binary(expression),
            ScalarExpr::UnaryMinus(inner) => {
                let source = self.context.semantic(self.context.infer(inner))?.flatten();
                let source = source
                    .map(|source| {
                        self.context.semantic(
                            crate::type_resolution::operators::unary_minus_result_type_with_control(
                                &source, control,
                            ),
                        )
                    })
                    .transpose()?
                    .flatten();
                expression.map_child(0, control, |child| {
                    let child = self.bind(child)?;
                    match source {
                        Some(source) => self.wrap_declared(child, &source),
                        None => Ok(child),
                    }
                })
            }
            ScalarExpr::Between { expr, low, high } => {
                let low_types = self
                    .context
                    .semantic(self.context.binary_operand_types(
                        crate::ast::BinaryOp::GreaterEqual,
                        expr,
                        low,
                    ))?
                    .flatten();
                let high_types = self
                    .context
                    .semantic(self.context.binary_operand_types(
                        crate::ast::BinaryOp::LessEqual,
                        expr,
                        high,
                    ))?
                    .flatten();
                expression = expression.map_children(control, |_, child| self.bind(child))?;
                self.coerce_between(expression, low_types, high_types)
            }
            ScalarExpr::InList { .. } => {
                expression = expression.map_children(control, |_, child| self.bind(child))?;
                self.coerce_membership(expression)
            }
            ScalarExpr::WindowCall { args, filter, .. } => {
                let argument_count = args.len();
                let has_filter = filter.is_some();
                let last = expression.child_count() - usize::from(has_filter);
                for index in 0..argument_count {
                    expression = expression.map_child(index, control, |child| self.bind(child))?;
                }
                if has_filter {
                    expression = expression.map_child(last, control, |child| self.bind(child))?;
                }
                // The remaining constructor slots are partition keys, ordering keys and frame offsets, in binding order.
                for index in argument_count..last {
                    expression = expression.map_child(index, control, |child| self.bind(child))?;
                }
                Ok(expression)
            }
            ScalarExpr::Case { .. } => self.bind_case(expression),
            ScalarExpr::Cast { .. } => self.bind_cast(expression),
            ScalarExpr::Collated { .. }
            | ScalarExpr::Row(_)
            | ScalarExpr::Not(_)
            | ScalarExpr::And(_)
            | ScalarExpr::Or(_)
            | ScalarExpr::IsNull { .. }
            | ScalarExpr::InSubquery { .. } => {
                expression.map_children(control, |_, child| self.bind(child))
            }
            _ => Ok(expression),
        }
    }

    fn bind_binary(&self, mut expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        let ScalarExpr::Binary { op, lhs, rhs } = &*expression else {
            unreachable!("binary operand binding");
        };
        let types = self
            .context
            .semantic(self.context.binary_operand_types(*op, lhs, rhs))?
            .flatten();
        let real = types.as_deref().map_or([false; 2], |types| {
            [
                matches!(types[0], ColumnType::Real),
                matches!(types[1], ColumnType::Real),
            ]
        });
        let arithmetic = matches!(
            op,
            crate::ast::BinaryOp::Add
                | crate::ast::BinaryOp::Subtract
                | crate::ast::BinaryOp::Multiply
                | crate::ast::BinaryOp::Divide
        );
        expression = expression.map_children(control, |_, child| self.bind(child))?;
        if let Some(types) = types {
            expression =
                expression.map_child(0, control, |child| self.operand_cast(child, &types[0]))?;
            expression =
                expression.map_child(1, control, |child| self.operand_cast(child, &types[1]))?;
        }
        if arithmetic {
            for (index, real) in real.into_iter().enumerate() {
                if real {
                    expression = expression.map_child(index, control, |child| {
                        self.wrap_declared(child, &ColumnType::Real)
                    })?;
                }
            }
        }
        Ok(expression)
    }
}
