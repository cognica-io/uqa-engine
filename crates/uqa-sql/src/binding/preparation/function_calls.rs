//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep call selection separate from input coercion so modifiers precede input effects.

use super::{ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::ast::{BinaryOp, FunctionBinding};
use crate::catalog::collations::{derivation::Derivation, CollationSelection};
use crate::semantics::sets::placement::SetPlacement;
use crate::type_resolution::ResolvedFunctionOverload;

pub(super) struct ObservedFunctionArguments {
    pub(super) values: Vec<ExpressionType>,
    pub(super) names: Vec<Option<String>>,
    pub(super) variadic: bool,
}

pub(super) struct ObservedFunctionModifiers {
    pub(super) diagnostics: Derivation,
    order_types: Vec<Option<ColumnType>>,
}

impl ObservedFunctionArguments {
    pub(super) fn retain_argument_observer(&self, binding: &mut FunctionBinding) {
        if !binding.builtin {
            return;
        }
        let [argument] = self.values.as_slice() else {
            return;
        };
        if binding.name == "pg_catalog.pg_typeof" {
            binding.dispatch = Some(crate::ast::FunctionDispatch::TypeOf {
                type_oid: argument.ty.as_ref().map_or(705, |ty| {
                    u32::try_from(crate::catalog::type_metadata::pg_type_oid(ty))
                        .expect("catalog type identity")
                }),
            });
            return;
        }
        if binding.name != "pg_catalog.pg_collation_for" {
            return;
        }
        let noncollatable_type_oid = argument.ty.as_ref().and_then(|ty| {
            (crate::catalog::type_metadata::pg_type_collation_oid(ty) == 0).then(|| {
                u32::try_from(crate::catalog::type_metadata::pg_type_oid(ty))
                    .expect("catalog type identity")
            })
        });
        binding.dispatch = Some(crate::ast::FunctionDispatch::CollationFor {
            collation_oid: u32::try_from(argument.derivation.oid()).expect("collation identity"),
            noncollatable_type_oid,
        });
    }

    pub(super) fn types(&self) -> Vec<Option<ColumnType>> {
        self.values.iter().map(|value| value.ty.clone()).collect()
    }

    /// `PostgreSQL`'s `inputcollid` for these coerced arguments, as `assign_collations_walker` merges the call's children: an explicit collation wins, a non-default implicit collation beats the default, and conflicting implicit collations leave zero. A collatable argument without a collation also leaves zero. A WITHIN GROUP call merges its direct arguments, and its aggregated argument only when it is the only one; a hypothetical-set call merges none of its paired arguments. `None` records that no argument is collatable.
    pub(super) fn input_collation(
        &self,
        direct: usize,
        kind: Option<super::super::ordered_calls::Kind>,
        within_group: bool,
    ) -> Result<Option<CollationSelection>, SQLError> {
        use super::super::ordered_calls::Kind;
        let contributing = if !within_group {
            &self.values[..]
        } else if matches!(kind, Some(Kind::Hypothetical)) {
            &self.values[..0]
        } else if self.values.len() == direct + 1 {
            &self.values[..]
        } else {
            &self.values[..direct.min(self.values.len())]
        };
        let merged = contributing.iter().fold(Derivation::None, |state, value| {
            state.combine(value.derivation)
        });
        if merged != Derivation::None {
            let oid = u32::try_from(merged.oid()).expect("collation identity");
            return CollationSelection::from_oid(oid).map(Some).ok_or_else(|| {
                SQLError::Internal(format!("cache lookup failed for collation {oid}"))
            });
        }
        Ok(contributing
            .iter()
            .any(|value| {
                value
                    .ty
                    .as_ref()
                    .is_some_and(crate::schema::composites::type_is_collatable)
            })
            .then_some(CollationSelection::INDETERMINATE))
    }

    pub(super) fn into_collated_result(
        mut self,
        ty: Option<ColumnType>,
        binding: Option<&FunctionBinding>,
        additional: Derivation,
        distinct: bool,
        ordered_start: Option<usize>,
    ) -> ExpressionType {
        for (position, value) in self.values.iter_mut().enumerate() {
            if distinct || ordered_start.is_some_and(|start| position >= start) {
                value.derivation = value.derivation.key();
            }
        }
        collated_result(ty, &self.values, binding, additional)
    }
}

pub(super) struct SelectedFunctionArguments {
    pub(super) overload: Option<ResolvedFunctionOverload>,
    positions: Option<Vec<usize>>,
    pub(super) kind: Option<super::super::ordered_calls::Kind>,
}

impl SelectedFunctionArguments {
    pub(super) fn validate_modifiers(
        &self,
        name: &str,
        modifiers: super::super::ordered_calls::Modifiers,
        arguments: &ObservedFunctionArguments,
    ) -> Result<(), SQLError> {
        let kind = self.kind.or(self
            .overload
            .as_ref()
            .map(|_| super::super::ordered_calls::Kind::Ordinary));
        kind.map_or(Ok(()), |kind| {
            super::super::ordered_calls::validate(
                name,
                kind,
                modifiers,
                &arguments.names,
                &arguments.types(),
            )
        })
    }
}

pub(super) enum FunctionResult {
    /// The original argument analysis selected a result, including a deferred UQA result.
    Selected(Option<ColumnType>),
    Unselected,
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
            Some(
                crate::ast::FunctionDispatch::NumericOperator(_)
                    | crate::ast::FunctionDispatch::Pattern { .. }
                    | crate::ast::FunctionDispatch::PatternEscape { .. }
            )
        ) || binding
            .and_then(|binding| binding.dispatch)
            .is_some_and(crate::ast::FunctionDispatch::is_concatenation)
            || binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax)
        {
            (None, None)
        } else if let Some(selected) = crate::type_resolution::json_path::resolve_call(
            name,
            binding,
            crate::type_resolution::RoutineCallDescriptor {
                argument_names: &arguments.names,
                argument_types: &types,
                explicit_variadic: arguments.variadic,
            },
            Some(self.routines),
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )? {
            (
                Some(
                    selected
                        .into_uncontrolled()
                        .expect("ordinary routine selection has no reservation"),
                ),
                None,
            )
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
            &self.parameters.values(),
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

    fn coerce_pattern_arguments(
        &mut self,
        dispatch: crate::ast::FunctionDispatch,
        arguments: &mut ObservedFunctionArguments,
    ) -> Result<FunctionResult, SQLError> {
        let types = arguments.types();
        if let Some(crate::ast::FunctionDispatch::PatternEscape { similar }) = Some(dispatch) {
            if !(1..=2).contains(&types.len()) {
                return Err(SQLError::Internal("invalid pattern escape operands".into()));
            }
            if arguments.values.iter().any(ExpressionType::is_deferred) {
                return Ok(FunctionResult::Selected(None));
            }
            let inputs = [
                types.first().and_then(Option::as_ref),
                types.get(1).and_then(Option::as_ref),
            ];
            let selected = crate::type_resolution::pattern::select_escape(
                similar,
                &inputs[..types.len()],
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )?;
            for value in &mut arguments.values {
                self.parameters.coerce_unknown(value, &selected)?;
            }
            return Ok(FunctionResult::Selected(Some(
                selected
                    .into_uncontrolled()
                    .expect("ordinary pattern helper selection"),
            )));
        }
        if let Some(crate::ast::FunctionDispatch::Pattern { operator, .. }) = Some(dispatch) {
            if arguments.values.iter().any(ExpressionType::is_deferred) {
                return Ok(FunctionResult::Selected(None));
            }
            if types.len() != 2 {
                return Err(SQLError::Internal(
                    "invalid pattern operator operands".into(),
                ));
            }
            let inputs = [
                types.first().and_then(Option::as_ref),
                types.get(1).and_then(Option::as_ref),
                types.get(2).and_then(Option::as_ref),
            ];
            let selected = crate::type_resolution::pattern::select(
                operator,
                &inputs[..types.len()],
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )?;
            for (position, value) in arguments.values.iter_mut().enumerate() {
                self.parameters
                    .coerce_unknown(value, selected.argument(position))?;
            }
            return Ok(FunctionResult::Selected(Some(ColumnType::Boolean)));
        }
        unreachable!("only native pattern dispatches enter pattern argument coercion")
    }

    pub(super) fn coerce_function_arguments(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        original_args: &[ScalarExpr],
        arguments: &mut ObservedFunctionArguments,
        selection: &SelectedFunctionArguments,
    ) -> Result<FunctionResult, SQLError> {
        if binding.is_none() && self.routines.has_untyped_function(name) {
            return Ok(FunctionResult::Selected(None));
        }
        if matches!(
            binding.and_then(|binding| binding.dispatch),
            Some(crate::ast::FunctionDispatch::IsDistinct)
        ) && matches!(original_args, [ScalarExpr::Row(_), ScalarExpr::Row(_)])
        {
            let [left, right] = arguments.values.as_mut_slice() else {
                return Err(SQLError::Internal(
                    "ROW DISTINCT analysis lost its arguments".into(),
                ));
            };
            self.coerce_row_comparison(BinaryOp::Equal, left, right, true)?;
            return Ok(FunctionResult::Selected(Some(ColumnType::Boolean)));
        }
        if let Some(
            dispatch @ (crate::ast::FunctionDispatch::Pattern { .. }
            | crate::ast::FunctionDispatch::PatternEscape { .. }),
        ) = binding.and_then(|binding| binding.dispatch)
        {
            return self.coerce_pattern_arguments(dispatch, arguments);
        }
        let types = arguments.types();
        if binding
            .and_then(|binding| binding.dispatch)
            .is_some_and(crate::ast::FunctionDispatch::is_concatenation)
        {
            let [left, right] = types.as_slice() else {
                return Err(SQLError::Internal("invalid concatenation operands".into()));
            };
            if arguments.values.iter().any(ExpressionType::is_deferred) {
                return Ok(FunctionResult::Selected(None));
            }
            let control = uqa_core::memory::ProductionControl::uncontrolled();
            let selected = crate::type_resolution::concatenation::select(
                left.as_ref(),
                right.as_ref(),
                &control,
            )?;
            for (position, value) in arguments.values.iter_mut().enumerate() {
                self.parameters
                    .coerce_unknown(value, selected.argument(position))?;
            }
            return Ok(FunctionResult::Selected(Some(
                selected
                    .into_result(&control)?
                    .into_uncontrolled()
                    .expect("ordinary concatenation selection"),
            )));
        }
        if let Some(crate::ast::FunctionDispatch::NumericOperator(operator)) =
            binding.and_then(|binding| binding.dispatch)
        {
            let selected = crate::type_resolution::numeric_operator_types(operator, &types)?;
            for (value, target) in arguments.values.iter_mut().zip(&selected.arguments) {
                self.parameters.coerce_unknown(value, target)?;
            }
            return Ok(FunctionResult::Selected(Some(selected.result)));
        }
        if binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax) {
            if name == "nullif" {
                if let [left, right] = arguments.values.as_mut_slice() {
                    let original = left.ty.clone();
                    let deferred = left.is_deferred() || right.is_deferred();
                    let [selected, _, _] = self.binary_types(BinaryOp::Equal, left, right)?;
                    let result = if deferred || selected == ColumnType::Record && original.is_some()
                    {
                        original
                    } else {
                        Some(selected)
                    };
                    return Ok(FunctionResult::Selected(result));
                }
            } else {
                let result = self.common(
                    crate::type_resolution::CommonTypeContext::function(name)
                        .unwrap_or(crate::type_resolution::CommonTypeContext::Coalesce),
                    &mut arguments.values,
                )?;
                return Ok(FunctionResult::Selected(result));
            }
            return Ok(FunctionResult::Unselected);
        }
        let targets = self.function_argument_targets(name, binding, selection, types)?;
        for (index, value) in arguments.values.iter_mut().enumerate() {
            let position = selection
                .positions
                .as_ref()
                .map_or(index, |positions| positions[index]);
            if let Some(target) = targets.get(position).and_then(Option::as_ref) {
                self.parameters.coerce_unknown(value, target)?;
            }
        }
        if let Some(ty) = ordered_result_type(selection, arguments)? {
            return Ok(FunctionResult::Selected(Some(ty)));
        }
        Ok(FunctionResult::Unselected)
    }

    /// Record `PostgreSQL`'s `inputcollid` on the binding the call keeps: its selected overload, its existing binding, or, for a regular-expression function without a registered signature, a built-in selected by name. Type and collation observers also keep the argument metadata they observe.
    pub(super) fn keep_call_selection(
        &mut self,
        expression: &ScalarExpr,
        arguments: &ObservedFunctionArguments,
        input_collation: Option<CollationSelection>,
        selected: Option<&mut ResolvedFunctionOverload>,
    ) {
        let ScalarExpr::Func { name, binding, .. } = expression else {
            unreachable!("function expression");
        };
        if let Some(selected) = selected {
            selected.binding.input_collation = input_collation;
            arguments.retain_argument_observer(&mut selected.binding);
        } else if binding.is_none()
            && crate::expr::binds_input_collation_by_name(name)
            && !self.routines.has_untyped_function(name)
        {
            self.parameters.retain_builtin_selection(
                expression,
                FunctionBinding::builtin_by_name(name, input_collation),
            );
        }
        self.parameters
            .retain_input_collation(expression, input_collation);
    }

    fn function_argument_targets(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        selection: &SelectedFunctionArguments,
        types: Vec<Option<ColumnType>>,
    ) -> Result<Vec<Option<ColumnType>>, SQLError> {
        Ok(if let Some(selected) = &selection.overload {
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
        } else if let Some(binding) = binding.filter(|binding| {
            binding.builtin && binding.dispatch.is_none() && !binding.argument_types.is_empty()
        }) {
            binding
                .argument_types
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
        })
    }
}

/// Substitute a selected ordered-set declaration from the argument occurrences already observed, after modifier checks and input coercion. The shared routine matcher preserves enum/domain/record identities instead of exposing a pseudo-type as the expression result.
fn ordered_result_type(
    selection: &SelectedFunctionArguments,
    arguments: &ObservedFunctionArguments,
) -> Result<Option<ColumnType>, SQLError> {
    use crate::type_resolution::{
        match_routine_signature, routine_polymorphic_type, RoutineCallDescriptor,
        RoutineParameterDescriptor,
    };
    if !matches!(
        selection.kind,
        Some(super::super::ordered_calls::Kind::OrderedSet {
            polymorphic: true,
            ..
        })
    ) {
        return Ok(None);
    }
    let Some(selected) = &selection.overload else {
        return Ok(None);
    };
    let ColumnType::Named(result) = &selected.return_type else {
        return Ok(None);
    };
    if routine_polymorphic_type(result).is_none() {
        return Ok(None);
    }
    let parameters = selected
        .binding
        .argument_types
        .iter()
        .map(|name| RoutineParameterDescriptor {
            name: None,
            type_name: name.clone(),
            column_type: None,
            has_default: false,
            default_type: None,
            variadic: false,
        })
        .collect::<Vec<_>>();
    let types = arguments.types();
    let matched = match_routine_signature(
        &parameters,
        RoutineCallDescriptor {
            argument_names: &arguments.names,
            argument_types: &types,
            explicit_variadic: arguments.variadic,
        },
    )
    .map_err(|error| {
        SQLError::Internal(format!(
            "selected ordered-set signature cannot resolve its result: {error:?}"
        ))
    })?
    .ok_or_else(|| {
        SQLError::Internal("selected ordered-set signature no longer matches its arguments".into())
    })?;
    matched.substitute_type(result).map(Some).ok_or_else(|| {
        SQLError::Internal("selected ordered-set result type remains polymorphic".into())
    })
}

/// Ordered and FILTER expressions are visited independently of a call's result collation.
impl Preparation<'_> {
    pub(super) fn function_modifier_collations(
        &mut self,
        orders: &[crate::ScalarOrder],
        filter: Option<&ScalarExpr>,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ObservedFunctionModifiers, SQLError> {
        let mut selected = Derivation::None;
        let mut order_types = Vec::with_capacity(orders.len());
        for order in orders {
            let value = self.expression(&order.expr, input, subqueries)?;
            selected = selected.combine(value.derivation.key().error_only());
            order_types.push(value.ty);
        }
        if let Some(filter) = filter {
            let filter = self.in_placement(SetPlacement::Filter, |analysis| {
                analysis.boolean_expression(filter, input, subqueries, "FILTER")
            })?;
            selected = selected.combine(filter.derivation.error_only());
        }
        Ok(ObservedFunctionModifiers {
            diagnostics: selected,
            order_types,
        })
    }

    pub(super) fn observed_call_type(
        &mut self,
        expression: &ScalarExpr,
        arguments: &ObservedFunctionArguments,
        modifiers: &ObservedFunctionModifiers,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<Option<ColumnType>, SQLError> {
        let (args, orders, filter) = match expression {
            ScalarExpr::Func {
                binding,
                args,
                order_by,
                filter,
                ..
            } => {
                if binding.as_ref().and_then(|binding| binding.dispatch)
                    == Some(crate::ast::FunctionDispatch::FieldSelect)
                {
                    return self.known_type(expression, input, subqueries);
                }
                (args.as_slice(), order_by.as_slice(), filter.as_deref())
            }
            ScalarExpr::WindowCall { args, filter, .. } => {
                (args.as_slice(), &[][..], filter.as_deref())
            }
            _ => return Err(SQLError::Internal("expected analyzed function call".into())),
        };
        let mut observations = crate::scalar_call_arguments(args)?
            .into_iter()
            .zip(&arguments.values)
            .map(|(argument, value)| (argument.value, value.ty.as_ref()))
            .collect::<Vec<_>>();
        for (position, order) in orders.iter().enumerate() {
            let ty = arguments
                .values
                .get(args.len() + position)
                .map(|value| value.ty.as_ref())
                .unwrap_or_else(|| modifiers.order_types[position].as_ref());
            observations.push((&order.expr, ty));
        }
        if let Some(filter) = filter {
            observations.push((filter, Some(&ColumnType::Boolean)));
        }
        let inputs = crate::type_resolution::CallInputTypes::new(input, observations);
        let parameters = self.parameters.values();
        if self.scope.validate_references {
            super::super::analysis::validate_call(
                self.routines,
                expression,
                &inputs,
                &parameters,
                self.routines,
            )?;
        }
        crate::type_resolution::call_type_from_observations(
            expression,
            &inputs,
            &parameters,
            self.routines,
        )
    }
}

pub(super) fn collated_result(
    ty: Option<ColumnType>,
    values: &[ExpressionType],
    binding: Option<&FunctionBinding>,
    additional: Derivation,
) -> ExpressionType {
    let mut value = ExpressionType::combined(ty, values);
    if matches!(
        binding.and_then(|binding| binding.dispatch),
        Some(crate::ast::FunctionDispatch::IsDistinct)
    ) {
        if let [left, right] = values {
            value.derivation = ExpressionType::comparison(left, right);
        }
    }
    value.derivation = value.derivation.combine(additional);
    value
}
