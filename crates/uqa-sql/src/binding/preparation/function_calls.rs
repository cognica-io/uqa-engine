//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep call selection separate from input coercion so modifiers precede input effects.

use super::{ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::ast::{BinaryOp, FunctionBinding};
use crate::type_resolution::ResolvedFunctionOverload;

pub(super) struct ObservedFunctionArguments {
    pub(super) values: Vec<ExpressionType>,
    pub(super) names: Vec<Option<String>>,
    pub(super) variadic: bool,
}

impl ObservedFunctionArguments {
    pub(super) fn types(&self) -> Vec<Option<ColumnType>> {
        self.values.iter().map(|value| value.ty.clone()).collect()
    }
}

pub(super) struct SelectedFunctionArguments {
    pub(super) overload: Option<ResolvedFunctionOverload>,
    positions: Option<Vec<usize>>,
    pub(super) kind: Option<super::super::ordered_calls::Kind>,
}

impl Preparation<'_> {
    /// Borrow the original direct and ordered children: their input leaf identities
    /// must survive until the retained constants are applied to the original plan.
    pub(super) fn observe_function_arguments(
        &mut self,
        name: &str,
        args: &[ScalarExpr],
        ordered: &[crate::ScalarOrder],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ObservedFunctionArguments, SQLError> {
        let arguments = crate::scalar_call_arguments(args)?;
        let mut values = arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                if crate::semantics::is_semantic_field_argument(name, args, index)? {
                    Ok(ExpressionType::resolved(Some(ColumnType::Text)))
                } else {
                    self.expression(argument.value, input, subqueries)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_string))
            .collect::<Vec<_>>();
        for order in ordered {
            values.push(self.expression(&order.expr, input, subqueries)?);
            names.push(None);
        }
        Ok(ObservedFunctionArguments {
            values,
            names,
            variadic: arguments.iter().any(|argument| argument.explicit_variadic),
        })
    }

    pub(super) fn select_function_arguments(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        args: &[ScalarExpr],
        arguments: &ObservedFunctionArguments,
    ) -> Result<SelectedFunctionArguments, SQLError> {
        if binding.is_none() && self.routines.has_untyped_function(name) {
            return Ok(SelectedFunctionArguments {
                overload: None,
                positions: None,
                kind: None,
            });
        }
        let types = arguments.types();
        let mut kind = None;
        let (overload, positions) = if matches!(
            binding.and_then(|binding| binding.dispatch),
            Some(crate::ast::FunctionDispatch::NumericOperator(_))
        ) || binding
            .is_some_and(FunctionBinding::is_polymorphic_builtin_syntax)
        {
            (None, None)
        } else if let Some(fixed) = crate::resolve_fixed_builtin_call(
            name,
            binding,
            &arguments.names,
            &types,
            arguments.variadic,
            Some(self.routines),
        )? {
            (Some(fixed.selected), fixed.builtin_argument_positions)
        } else if let Some(array) = crate::type_resolution::resolve_array_transform_call(
            name,
            binding,
            args,
            &types,
            arguments.variadic,
            self.routines,
        )? {
            kind = Some(super::super::ordered_calls::Kind::Ordinary);
            (array.overload, array.builtin_argument_positions)
        } else {
            (
                self.routines.resolve_function_overload(
                    name,
                    binding,
                    &arguments.names,
                    &types,
                    arguments.variadic,
                )?,
                None,
            )
        };
        if let Some(selected) = &overload {
            self.scope.record_routine_dependency(&selected.binding);
        }
        Ok(SelectedFunctionArguments {
            overload,
            positions,
            kind,
        })
    }

    pub(super) fn select_ordered_function_arguments(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        args: &[ScalarExpr],
        ordered: &[crate::ScalarOrder],
        arguments: &ObservedFunctionArguments,
    ) -> Result<SelectedFunctionArguments, SQLError> {
        if let Some((selected, kind)) = super::super::ordered_calls::resolve(
            name,
            binding,
            &arguments.names,
            &arguments.types(),
            arguments.variadic,
            self.routines,
        )? {
            self.scope.record_routine_dependency(&selected.binding);
            Ok(SelectedFunctionArguments {
                overload: Some(selected),
                positions: None,
                kind: Some(kind),
            })
        } else {
            let args = if ordered.is_empty() {
                std::borrow::Cow::Borrowed(args)
            } else {
                std::borrow::Cow::Owned(
                    args.iter()
                        .chain(ordered.iter().map(|order| &order.expr))
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            };
            self.select_function_arguments(name, binding, &args, arguments)
        }
    }

    pub(super) fn coerce_function_arguments(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &mut ObservedFunctionArguments,
        selection: &SelectedFunctionArguments,
    ) -> Result<(), SQLError> {
        if binding.is_none() && self.routines.has_untyped_function(name) {
            return Ok(());
        }
        let types = arguments.types();
        if let Some(crate::ast::FunctionDispatch::NumericOperator(operator)) =
            binding.and_then(|binding| binding.dispatch)
        {
            let selected = crate::type_resolution::numeric_operator_types(operator, &types)?;
            for (value, target) in arguments.values.iter_mut().zip(&selected.arguments) {
                self.parameters.coerce_unknown(value, target)?;
            }
            return Ok(());
        }
        if binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax) {
            if name == "nullif" {
                if let [left, right] = arguments.values.as_mut_slice() {
                    self.binary(BinaryOp::Equal, left, right)?;
                }
            } else {
                self.common(
                    crate::type_resolution::CommonTypeContext::function(name)
                        .unwrap_or(crate::type_resolution::CommonTypeContext::Coalesce),
                    &mut arguments.values,
                )?;
            }
            return Ok(());
        }
        let targets = if let Some(selected) = &selection.overload {
            let types = selected
                .binding
                .invocation
                .as_ref()
                .map_or(&selected.binding.argument_types, |invocation| {
                    &invocation.argument_targets
                });
            types
                .iter()
                .map(|name| {
                    if super::super::ordered_calls::is_polymorphic(name) {
                        Ok(None)
                    } else {
                        self.type_name(name).map(Some)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?
        } else if matches!(name, "cypher" | "ag_catalog.cypher") {
            [
                ColumnType::Name,
                ColumnType::Text,
                self.type_name("ag_catalog.agtype")?,
            ]
            .into_iter()
            .take(types.len())
            .map(Some)
            .collect()
        } else {
            let declared = if let Some(positions) = &selection.positions {
                let mut declared = vec![None; types.len()];
                for (ty, position) in types.into_iter().zip(positions) {
                    declared[*position] = ty;
                }
                declared
            } else {
                types
            };
            crate::type_resolution::builtin_function_argument_targets(name, &declared)
        };
        for (index, value) in arguments.values.iter_mut().enumerate() {
            let position = selection
                .positions
                .as_ref()
                .map_or(index, |positions| positions[index]);
            if let Some(target) = targets.get(position).and_then(Option::as_ref) {
                self.parameters.coerce_unknown(value, target)?;
            }
        }
        Ok(())
    }
}
