//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Calls retain selected metadata and child owners through lazy argument and collation rewrites.

use super::{conditional, constants, Context, OwnedScalarExpr, SQLError, ScalarExpr};
use uqa_sql::type_resolution::OwnedBindingCall;

impl Context<'_, '_> {
    pub(super) fn call(
        &self,
        mut expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.config.production;
        if matches!(&*expression, ScalarExpr::Func { distinct: false, order_by, filter: None, .. } if order_by.is_empty())
        {
            if let Some(context) = &self.config.routine_inlining {
                expression = context.materialize_owned_call(expression, control)?;
            }
        }
        let mut call = OwnedBindingCall::new(expression, control)?;
        for index in 0..call.order_by().len() {
            call = call.map_order(index, control, |child| self.slot(child))?;
        }
        let row_distinct = call.binding().is_some_and(|binding| {
            binding.dispatch == Some(uqa_sql::ast::FunctionDispatch::RowDistinct)
        });
        let coalesce = !row_distinct && constants::is_coalesce(call.name(), call.binding());
        if row_distinct {
            let (parent, arguments) = call.take_arguments(control)?;
            let arguments = conditional::optimize_owned_boolean(
                arguments,
                false,
                self.config,
                control,
                |child| self.scalar(child),
            )?;
            if !matches!(&*arguments, ScalarExpr::Or(_)) {
                drop(parent);
                return Ok(arguments);
            }
            call = parent.replace_arguments(arguments, control)?;
        } else if !coalesce {
            call = call.map_arguments(control, |_, child| self.scalar(child))?;
        }
        let expression = if coalesce {
            conditional::optimize_owned_coalesce(
                call.into_expression(),
                self.config,
                control,
                |child| self.scalar(child),
            )?
        } else {
            call.into_expression()
        };
        let call = OwnedBindingCall::new(expression, control)?
            .map_filter(control, |child| self.scalar(child))?;
        self.finish(call.into_expression(), false, false, None)
    }

    pub(super) fn collated(
        &self,
        expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.config.production;
        let selected_arm = matches!(&*expression, ScalarExpr::Collated { expr, .. } if matches!(expr.as_ref(), ScalarExpr::Case { .. }));
        let expression = expression.map_child_with_parent(0, control, |parent, child| {
            let ScalarExpr::Collated { ty, .. } = parent else {
                unreachable!("collated parent")
            };
            if matches!(&*child, ScalarExpr::Binary { .. }) {
                self.noncall(child, Some(ty))
            } else {
                self.scalar(child)
            }
        })?;
        let ScalarExpr::Collated {
            expr,
            ty,
            collation,
        } = &*expression
        else {
            unreachable!("retained collation expression")
        };
        let Some(oid) = collation.implicit_oid() else {
            return Ok(expression);
        };
        let retained = match expr.as_ref() {
            ScalarExpr::TypedLiteral { bound_type, .. } => {
                let inner = match bound_type.as_ref() {
                    Some(inner) if selected_arm => {
                        let inner_base = inner.without_type_modifiers_with_control(control)?;
                        let outer_base = ty.without_type_modifiers_with_control(control)?;
                        (*inner_base == *outer_base).then_some(inner)
                    }
                    _ => None,
                };
                inner.unwrap_or(ty)
            }
            ScalarExpr::Literal(_) => ty,
            _ => return Ok(expression),
        };
        let declaration = retained.catalog_name_with_control(control)?;
        let retained = retained.clone_with_control(control)?;
        let expression = expression.select_child(0);
        if matches!(&*expression, ScalarExpr::Literal(_)) {
            Ok(OwnedScalarExpr::typed_literal(
                expression.into_literal_value(control)?,
                declaration,
                Some(retained),
                Some(oid),
                None,
                control,
            )?)
        } else {
            Ok(expression.replace_literal_type(declaration, retained, Some(oid), control)?)
        }
    }
}
