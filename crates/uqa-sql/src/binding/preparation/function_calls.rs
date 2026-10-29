//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep call selection separate from input coercion so modifiers precede input effects.

use super::ownership::{AnalysisType, AnalysisVec};
use super::{ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::ast::{BinaryOp, FunctionBinding};
use crate::catalog::collations::{derivation::Derivation, CollationSelection};
use crate::semantics::sets::placement::SetPlacement;
use crate::type_resolution::ResolvedFunctionOverload;
use uqa_core::memory::{OwnedProductionControl, Produced, ProductionVec};

pub(super) struct ObservedFunctionArguments {
    pub(super) values: AnalysisVec<ExpressionType>,
    pub(super) names: Produced<Vec<Option<String>>>,
    pub(super) variadic: bool,
    control: OwnedProductionControl,
}

pub(super) struct ObservedFunctionModifiers {
    pub(super) diagnostics: Derivation,
    order_types: AnalysisVec<Option<AnalysisType>>,
}

impl ObservedFunctionArguments {
    pub(super) fn retain_argument_observer(&self, binding: &mut FunctionBinding) {
        if !binding.builtin {
            return;
        }
        let [argument] = &self.values[..] else {
            return;
        };
        if binding.name == "pg_catalog.pg_typeof" {
            binding.dispatch = Some(crate::ast::FunctionDispatch::TypeOf {
                type_oid: argument.ty.as_deref().map_or(705, |ty| {
                    u32::try_from(crate::catalog::type_metadata::pg_type_oid(ty))
                        .expect("catalog type identity")
                }),
            });
            return;
        }
        if binding.name != "pg_catalog.pg_collation_for" {
            return;
        }
        let noncollatable_type_oid = argument.ty.as_deref().and_then(|ty| {
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

    pub(super) fn types(&self) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
        let control = self.control.control();
        let mut output = ProductionVec::new(control);
        output.reserve(self.values.len())?;
        for value in &self.values {
            let (ty, memory) = match value.ty.as_deref() {
                Some(ty) => {
                    let (ty, memory) = ty.clone_with_control(&control)?.into_parts();
                    (Some(ty), memory)
                }
                None => (None, control.empty_reservation()),
            };
            output.push_produced(control.finish(ty, memory)?)?;
        }
        Ok(output.finish()?)
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
                    .as_deref()
                    .is_some_and(crate::schema::composites::type_is_collatable)
            })
            .then_some(CollationSelection::INDETERMINATE))
    }

    pub(super) fn into_collated_result(
        mut self,
        ty: Option<AnalysisType>,
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
    pub(super) overload: Option<Produced<ResolvedFunctionOverload>>,
    positions: Option<Produced<Vec<usize>>>,
    pub(super) kind: Option<super::super::ordered_calls::Kind>,
}

impl SelectedFunctionArguments {
    pub(super) fn validate_call_modifiers(
        &self,
        expression: &ScalarExpr,
        within_group: bool,
        arguments: &ObservedFunctionArguments,
    ) -> Result<(), SQLError> {
        let ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            distinct,
            filter,
            ..
        } = expression
        else {
            unreachable!("function modifier scope");
        };
        self.validate_modifiers(
            name,
            binding.as_ref(),
            args,
            super::super::ordered_calls::Modifiers {
                direct: super::super::ordered_calls::DirectArguments::from(args.as_slice()),
                ordered: order_by.len(),
                within_group,
                distinct: *distinct,
                filtered: filter.is_some(),
            },
            arguments,
        )
    }

    pub(super) fn validate_modifiers(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        args: &[crate::ScalarExpr],
        modifiers: super::super::ordered_calls::Modifiers,
        arguments: &ObservedFunctionArguments,
    ) -> Result<(), SQLError> {
        let types = arguments.types()?;
        let kind = self.kind.or(self
            .overload
            .as_ref()
            .map(|_| super::super::ordered_calls::Kind::Ordinary));
        kind.map_or(Ok(()), |kind| {
            super::super::ordered_calls::validate(name, kind, modifiers, &arguments.names, &types)
        })?;
        super::super::ordered_calls::validate_builtin_aggregate_arguments(
            name,
            self.overload
                .as_ref()
                .map(|selected| &selected.binding)
                .or(binding),
            args,
            || {
                crate::type_resolution::function_resolution_error(
                    "42883",
                    name,
                    &arguments.names,
                    &types,
                    "does not exist",
                )
            },
        )
    }
}

pub(super) enum FunctionResult {
    /// The original argument analysis selected a result, including a deferred UQA result.
    Selected(Option<AnalysisType>),
    Bound(AnalysisType, Produced<FunctionBinding>),
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
        let owner = self.scope.control().capture();
        let control = owner.control();
        let arguments = crate::ir::scalar_call_arguments_with_control(args, &control)?;
        let mut values = AnalysisVec::new(&control);
        values.reserve(arguments.len() + ordered.len(), &control)?;
        let mut names = ProductionVec::new(control);
        names.reserve(arguments.len() + ordered.len())?;
        for (index, argument) in arguments.iter().enumerate() {
            let value = if crate::semantics::is_semantic_field_argument(name, args, index)? {
                ExpressionType::resolved(Some(ColumnType::Text))
            } else {
                self.expression(argument.value, input, subqueries)?
            };
            values.push(value, &control)?;
            let (name, memory) = match argument.name {
                Some(name) => {
                    let (name, memory) = control.copy_text(name)?.into_parts();
                    (Some(name), memory)
                }
                None => (None, control.empty_reservation()),
            };
            names.push_produced(control.finish(name, memory)?)?;
        }
        for order in ordered {
            values.push(self.expression(&order.expr, input, subqueries)?, &control)?;
            names.push_produced(control.finish(None, control.empty_reservation())?)?;
        }
        Ok(ObservedFunctionArguments {
            values,
            names: names.finish()?,
            variadic: arguments.iter().any(|argument| argument.explicit_variadic),
            control: owner,
        })
    }

    pub(super) fn select_function_arguments(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        args: &[ScalarExpr],
        arguments: &ObservedFunctionArguments,
    ) -> Result<SelectedFunctionArguments, SQLError> {
        if binding.is_none()
            && self
                .routines
                .has_untyped_function_with_control(name, &self.scope.control())?
        {
            return Ok(SelectedFunctionArguments {
                overload: None,
                positions: None,
                kind: None,
            });
        }
        let types = arguments.types()?;
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
            &self.scope.control(),
        )? {
            (Some(selected), None)
        } else if let Some(fixed) = crate::type_resolution::resolve_fixed_builtin_call_with_control(
            name,
            binding,
            &arguments.names,
            &types,
            arguments.variadic,
            Some(self.routines),
            &self.scope.control(),
        )? {
            (Some(fixed.selected), fixed.builtin_argument_positions)
        } else if let Some(array) =
            crate::type_resolution::resolve_array_transform_call_with_control(
                name,
                binding,
                crate::type_resolution::ArrayTransformArguments {
                    expressions: args,
                    types: &types,
                    parameters: &self.parameters.values()?,
                    explicit_variadic: arguments.variadic,
                },
                self.routines,
                &self.scope.control(),
            )?
        {
            kind = Some(super::super::ordered_calls::Kind::Ordinary);
            (array.overload, array.builtin_argument_positions)
        } else {
            (
                self.routines.resolve_function_overload_with_control(
                    name,
                    binding,
                    &arguments.names,
                    &types,
                    arguments.variadic,
                    &self.scope.control(),
                )?,
                None,
            )
        };
        if let Some(selected) = &overload {
            self.scope.record_routine_dependency(&selected.binding)?;
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
        if let Some((selected, kind)) = super::super::ordered_calls::resolve_with_control(
            name,
            binding,
            &arguments.names,
            &arguments.types()?,
            arguments.variadic,
            self.routines,
            &self.scope.control(),
        )? {
            self.scope.record_routine_dependency(&selected.binding)?;
            Ok(SelectedFunctionArguments {
                overload: Some(selected),
                positions: None,
                kind: Some(kind),
            })
        } else if ordered.is_empty() {
            self.select_function_arguments(name, binding, args, arguments)
        } else {
            let control = self.scope.control();
            let mut combined = ProductionVec::new(control);
            combined.reserve(args.len() + ordered.len())?;
            for argument in args.iter().chain(ordered.iter().map(|order| &order.expr)) {
                combined.push_produced(argument.copy_for_rewrite(&control)?.finish(&control)?)?;
            }
            let combined = combined.finish()?;
            self.select_function_arguments(name, binding, &combined, arguments)
        }
    }

    fn coerce_pattern_arguments(
        &mut self,
        dispatch: crate::ast::FunctionDispatch,
        arguments: &mut ObservedFunctionArguments,
    ) -> Result<FunctionResult, SQLError> {
        let types = arguments.types()?;
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
                &self.scope.control(),
            )?;
            for value in &mut arguments.values {
                self.parameters.coerce_unknown(value, &selected)?;
            }
            return Ok(FunctionResult::Selected(Some(AnalysisType::from_produced(
                selected,
                &self.scope.control(),
            )?)));
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
                &self.scope.control(),
            )?;
            for (position, value) in arguments.values.iter_mut().enumerate() {
                self.parameters
                    .coerce_unknown(value, selected.argument(position))?;
            }
            return Ok(FunctionResult::Selected(Some(AnalysisType::Inline(
                ColumnType::Boolean,
            ))));
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
        if binding.is_none()
            && self
                .routines
                .has_untyped_function_with_control(name, &self.scope.control())?
        {
            return Ok(FunctionResult::Selected(None));
        }
        if matches!(
            binding.and_then(|binding| binding.dispatch),
            Some(crate::ast::FunctionDispatch::IsDistinct)
        ) && matches!(original_args, [ScalarExpr::Row(_), ScalarExpr::Row(_)])
        {
            let [left, right] = &mut arguments.values[..] else {
                return Err(SQLError::Internal(
                    "ROW DISTINCT analysis lost its arguments".into(),
                ));
            };
            self.coerce_row_comparison(BinaryOp::Equal, left, right, true)?;
            return Ok(FunctionResult::Selected(Some(AnalysisType::Inline(
                ColumnType::Boolean,
            ))));
        }
        if let Some(
            dispatch @ (crate::ast::FunctionDispatch::Pattern { .. }
            | crate::ast::FunctionDispatch::PatternEscape { .. }),
        ) = binding.and_then(|binding| binding.dispatch)
        {
            return self.coerce_pattern_arguments(dispatch, arguments);
        }
        if binding
            .and_then(|binding| binding.dispatch)
            .is_some_and(crate::ast::FunctionDispatch::is_concatenation)
        {
            return self.coerce_concatenation_arguments(arguments);
        }
        let types = arguments.types()?;
        if let Some(crate::ast::FunctionDispatch::NumericOperator(operator)) =
            binding.and_then(|binding| binding.dispatch)
        {
            let selected = crate::type_resolution::numeric_operator_types_with_control(
                operator,
                &types,
                &self.scope.control(),
            )?;
            for (value, target) in arguments.values.iter_mut().zip(&selected.arguments) {
                self.parameters.coerce_unknown(value, target)?;
            }
            return Ok(FunctionResult::Selected(Some(AnalysisType::from_produced(
                selected.result.clone_with_control(&self.scope.control())?,
                &self.scope.control(),
            )?)));
        }
        if binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax) {
            if name == "nullif" {
                if let [left, right] = &mut arguments.values[..] {
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

    fn coerce_concatenation_arguments(
        &mut self,
        arguments: &mut ObservedFunctionArguments,
    ) -> Result<FunctionResult, SQLError> {
        let types = arguments.types()?;
        let [left, right] = types.as_slice() else {
            return Err(SQLError::Internal("invalid concatenation operands".into()));
        };
        if arguments.values.iter().any(ExpressionType::is_deferred) {
            return Ok(FunctionResult::Selected(None));
        }
        let owner = self.scope.control().capture();
        let control = owner.control();
        let selected =
            crate::type_resolution::concatenation::select(left.as_ref(), right.as_ref(), &control)?;
        for (position, value) in arguments.values.iter_mut().enumerate() {
            self.parameters
                .coerce_unknown(value, selected.argument(position))?;
        }
        let binding = selected.binding(&control)?;
        let result = AnalysisType::from_produced(selected.into_result(&control)?, &control)?;
        Ok(FunctionResult::Bound(result, binding))
    }

    /// Record `PostgreSQL`'s `inputcollid` on the binding the call keeps: its selected overload, its existing binding, or, for a regular-expression function without a registered signature, a built-in selected by name. Type and collation observers also keep the argument metadata they observe.
    pub(super) fn keep_call_selection(
        &mut self,
        expression: &ScalarExpr,
        arguments: &ObservedFunctionArguments,
        input_collation: Option<CollationSelection>,
        selected: Option<Produced<ResolvedFunctionOverload>>,
    ) -> Result<Option<Produced<ResolvedFunctionOverload>>, SQLError> {
        let ScalarExpr::Func { name, binding, .. } = expression else {
            unreachable!("function expression");
        };
        let control = self.scope.control();
        let selected = if let Some(selected) = selected {
            let (mut selected, memory) = selected.into_parts();
            selected.binding.input_collation = input_collation;
            arguments.retain_argument_observer(&mut selected.binding);
            Some(control.finish(selected, memory)?)
        } else {
            if binding.is_none()
                && crate::expr::binds_input_collation_by_name(name)
                && !self
                    .routines
                    .has_untyped_function_with_control(name, &control)?
            {
                let selected =
                    FunctionBinding::builtin_by_name_with_control(name, input_collation, &control)?;
                self.parameters
                    .retain_builtin_selection(expression, selected)?;
            }
            None
        };
        self.parameters
            .retain_input_collation(expression, input_collation)?;
        Ok(selected)
    }

    fn function_argument_targets(
        &mut self,
        name: &str,
        binding: Option<&FunctionBinding>,
        selection: &SelectedFunctionArguments,
        types: Produced<Vec<Option<ColumnType>>>,
    ) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
        let control = self.scope.control();
        let declarations = selection
            .overload
            .as_ref()
            .map(|selected| {
                selected
                    .binding
                    .invocation
                    .as_ref()
                    .map_or(&selected.binding.argument_types, |invocation| {
                        &invocation.argument_targets
                    })
            })
            .or_else(|| {
                binding
                    .filter(|binding| {
                        binding.builtin
                            && binding.dispatch.is_none()
                            && !binding.argument_types.is_empty()
                    })
                    .map(|binding| &binding.argument_types)
            });
        let mut output = ProductionVec::new(control);
        if let Some(declarations) = declarations {
            output.reserve(declarations.len())?;
            for name in declarations {
                let ty = if super::super::ordered_calls::is_polymorphic(name) {
                    None
                } else {
                    Some(resolve_type_name(self.routines, name, &control)?)
                };
                output.push_produced(optional_type(ty, &control)?)?;
            }
            return Ok(output.finish()?);
        }
        if matches!(name, "cypher" | "ag_catalog.cypher") {
            let agtype = resolve_type_name(self.routines, "ag_catalog.agtype", &control)?;
            output.reserve(types.len().min(3))?;
            for ty in [&ColumnType::Name, &ColumnType::Text, &*agtype]
                .into_iter()
                .take(types.len())
            {
                output.push_produced(optional_type(
                    Some(ty.clone_with_control(&control)?),
                    &control,
                )?)?;
            }
            return Ok(output.finish()?);
        }
        let declared = if let Some(positions) = &selection.positions {
            output.reserve(types.len())?;
            for destination in 0..types.len() {
                let source = positions
                    .iter()
                    .position(|position| *position == destination);
                let ty = source
                    .and_then(|source| types[source].as_ref())
                    .map(|ty| ty.clone_with_control(&control))
                    .transpose()?;
                output.push_produced(optional_type(ty, &control)?)?;
            }
            output.finish()?
        } else {
            types
        };
        crate::type_resolution::builtin_function_argument_targets_with_control(
            name, &declared, &control,
        )
    }
}

/// Substitute a selected ordered-set declaration from the argument occurrences already observed, after modifier checks and input coercion. The shared routine matcher preserves enum/domain/record identities instead of exposing a pseudo-type as the expression result.
fn ordered_result_type(
    selection: &SelectedFunctionArguments,
    arguments: &ObservedFunctionArguments,
) -> Result<Option<AnalysisType>, SQLError> {
    use crate::type_resolution::{
        match_routine_candidate_with_control, routine_polymorphic_type_with_control,
        RoutineCallDescriptor, RoutineParameterDescriptor, RoutineSignatureProductionError,
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
    let control = arguments.control.control();
    if routine_polymorphic_type_with_control(result, &control)?.is_none() {
        return Ok(None);
    }
    let mut parameters = ProductionVec::new(control);
    parameters.reserve(selected.binding.argument_types.len())?;
    for name in &selected.binding.argument_types {
        let (type_name, memory) = control.copy_text(name)?.into_parts();
        parameters.push_produced(control.finish(
            RoutineParameterDescriptor {
                name: None,
                type_name,
                column_type: None,
                has_default: false,
                default_type: None,
                variadic: false,
            },
            memory,
        )?)?;
    }
    let parameters = parameters.finish()?;
    let types = arguments.types()?;
    let matched = match_routine_candidate_with_control(
        &parameters,
        RoutineCallDescriptor {
            argument_names: &arguments.names,
            argument_types: &types,
            explicit_variadic: arguments.variadic,
        },
        &control,
    )
    .map_err(|error| match error {
        RoutineSignatureProductionError::Production(error) => error,
        RoutineSignatureProductionError::Signature(error) => SQLError::Internal(format!(
            "selected ordered-set signature cannot resolve its result: {:?}",
            &*error
        )),
    })?
    .ok_or_else(|| {
        SQLError::Internal("selected ordered-set signature no longer matches its arguments".into())
    })?;
    let result = matched
        .substitute_type_with_control(result, &control)?
        .ok_or_else(|| {
            SQLError::Internal("selected ordered-set result type remains polymorphic".into())
        })?;
    Ok(Some(AnalysisType::from_produced(result, &control)?))
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
        let owner = self.scope.control().capture();
        let control = owner.control();
        let mut order_types = AnalysisVec::new(&control);
        order_types.reserve(orders.len(), &control)?;
        for order in orders {
            let value = self.expression(&order.expr, input, subqueries)?;
            selected = selected.combine(value.derivation.key().error_only());
            order_types.push(value.ty, &control)?;
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
    ) -> Result<Option<AnalysisType>, SQLError> {
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
        let owner = self.scope.control().capture();
        let control = owner.control();
        let call_arguments = crate::ir::scalar_call_arguments_with_control(args, &control)?;
        let observations = call_arguments
            .iter()
            .zip(&arguments.values)
            .map(|(argument, value)| (argument.value, value.ty.as_deref()))
            .chain(orders.iter().enumerate().map(|(position, order)| {
                let ty = arguments
                    .values
                    .get(args.len() + position)
                    .map(|value| value.ty.as_deref())
                    .unwrap_or_else(|| modifiers.order_types[position].as_deref());
                (&order.expr, ty)
            }))
            .chain(filter.map(|filter| (filter, Some(&ColumnType::Boolean))));
        let inputs = crate::type_resolution::CallInputTypes::new_with_control(
            input,
            observations,
            &control,
        )?;
        let parameters = self.parameters.values()?;
        if self.scope.validate_references {
            super::super::analysis::validate_call(
                self.routines,
                expression,
                &*inputs,
                &parameters,
                self.routines,
            )?;
        }
        crate::type_resolution::call_type_from_observations_with_control(
            expression,
            &inputs,
            &parameters,
            self.routines,
            &control,
        )?
        .map(|ty| AnalysisType::from_produced(ty, &control))
        .transpose()
    }
}

pub(super) fn collated_result(
    ty: Option<AnalysisType>,
    values: &[ExpressionType],
    binding: Option<&FunctionBinding>,
    additional: Derivation,
) -> ExpressionType {
    let mut value = ExpressionType::combined_owned(ty, values);
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

fn optional_type(
    ty: Option<Produced<ColumnType>>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Produced<Option<ColumnType>>, SQLError> {
    let (ty, memory) = ty.map_or((None, control.empty_reservation()), |ty| {
        let (ty, memory) = ty.into_parts();
        (Some(ty), memory)
    });
    Ok(control.finish(ty, memory)?)
}
fn resolve_type_name(
    resolver: &dyn crate::FunctionTypeResolver,
    name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    resolver
        .resolve_type_name_with_control(name, control)?
        .map_or_else(|| ColumnType::from_sql_name_with_control(name, control), Ok)
}
