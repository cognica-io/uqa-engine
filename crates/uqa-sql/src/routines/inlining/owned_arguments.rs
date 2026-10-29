//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation assembly keeps argument payloads and temporary slots independently owned.

use super::RoutineInliningContext;
use crate::{
    ast::{CreateFunction, RoutineInvocationBinding, RoutineVariadicMode},
    plan::{ExpressionPlan, OwnedScalarExpr},
    ColumnType, SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{MemoryReservation, ProductionControl, ProductionVec},
    Value,
};

pub(super) struct OwnedArguments {
    values: Vec<OwnedScalarExpr>,
    memory: Option<MemoryReservation>,
}

struct Slots {
    values: Vec<Option<OwnedScalarExpr>>,
    _memory: Option<MemoryReservation>,
}

pub(super) struct ArgumentIterator {
    values: std::vec::IntoIter<OwnedScalarExpr>,
    _memory: Option<MemoryReservation>,
}

impl Iterator for ArgumentIterator {
    type Item = OwnedScalarExpr;
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl ExactSizeIterator for ArgumentIterator {}

impl OwnedArguments {
    pub(super) fn copy_from(
        arguments: &[ScalarExpr],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut output = ProductionVec::new(*control);
        output.reserve(arguments.len())?;
        for argument in arguments {
            let expression = argument.copy_for_rewrite(control)?;
            output.push_produced(control.finish(expression, control.empty_reservation())?)?;
        }
        let (values, memory) = output.finish()?.into_parts();
        Ok(Self { values, memory })
    }

    pub(super) fn into_iter(self) -> ArgumentIterator {
        ArgumentIterator {
            values: self.values.into_iter(),
            _memory: self.memory,
        }
    }

    pub(super) fn into_container(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        Ok(OwnedScalarExpr::boolean(self.into_iter(), false, control)?)
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered argument materialization keeps positional defaults and variadic coercions at the same selection boundary"
)]
pub(super) fn prepare(
    context: &RoutineInliningContext<'_>,
    definition: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    arguments: &[ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedArguments>, SQLError> {
    let arguments = crate::ir::scalar_call_arguments_with_control(arguments, control)?;
    if invocation.argument_positions.len() != arguments.len()
        || invocation.argument_targets.len() != arguments.len()
        || invocation.parameter_types.len() != definition.params.len()
    {
        return Err(SQLError::Internal(
            "inconsistent routine invocation during expansion".into(),
        ));
    }
    let expanded = match invocation.variadic_mode {
        RoutineVariadicMode::Expanded { parameter_index } => Some(parameter_index),
        RoutineVariadicMode::None | RoutineVariadicMode::Explicit { .. } => None,
    };
    let mut slots = slot_buffer(definition.params.len(), control)?;
    let mut variadic = ProductionVec::new(*control);
    let variadic_count = invocation
        .argument_positions
        .iter()
        .filter(|position| Some(**position) == expanded)
        .count();
    variadic.reserve(variadic_count)?;
    for (index, (argument, position)) in arguments
        .iter()
        .zip(&invocation.argument_positions)
        .enumerate()
    {
        control.check()?;
        let source = invocation
            .argument_sources
            .get(index)
            .and_then(Option::as_deref)
            .map(|name| {
                context
                    .types
                    .resolve_catalog_column_type_name_with_control(name, control)
            })
            .transpose()?;
        let argument = coerce(
            context,
            argument.value.copy_for_rewrite(control)?,
            source.as_deref(),
            &invocation.argument_targets[index],
            control,
        )?;
        if Some(*position) == expanded {
            variadic.push_produced(control.finish(argument, control.empty_reservation())?)?;
        } else {
            let slot = slots.values.get_mut(*position).ok_or_else(|| {
                SQLError::Internal("routine parameter index is out of range".into())
            })?;
            if slot.replace(argument).is_some() {
                return Err(SQLError::Internal(
                    "duplicate routine argument during expansion".into(),
                ));
            }
        }
    }
    if let Some(position) = expanded {
        let Some(slot) = slots.values.get_mut(position) else {
            return Err(SQLError::Internal(
                "variadic parameter index is out of range".into(),
            ));
        };
        let (values, memory) = variadic.finish()?.into_parts();
        let array = OwnedScalarExpr::array(OwnedArguments { values, memory }.into_iter(), control)?;
        *slot = Some(OwnedScalarExpr::cast(
            array,
            control.copy_text(&invocation.parameter_types[position])?,
            true,
            control,
        )?);
    }
    let mut result = ProductionVec::new(*control);
    result.reserve(definition.identity_arity())?;
    for (position, parameter) in definition.params.iter().enumerate() {
        control.check()?;
        if !super::super::body_parameters::is_sql_body_parameter(parameter) {
            continue;
        }
        let argument = if let Some(argument) = slots.values[position].take() {
            argument
        } else {
            let Some(default) = &parameter.default else {
                return Err(SQLError::Internal(
                    "required routine argument disappeared during expansion".into(),
                ));
            };
            let plan = match ExpressionPlan::lower_scalar_for_rewrite(default, control) {
                Ok(plan) => plan,
                Err(crate::schema::retention::CatalogRetentionError::UnexpectedSubquery) => {
                    return Ok(None)
                }
                Err(error) => return Err(error.into()),
            };
            if crate::semantics::sets::static_setness::expression_may_return_set_statically(
                context.routines,
                &plan,
            ) {
                return Ok(None);
            }
            let source = crate::type_resolution::scalar_type_with_resolver_and_control(
                &plan,
                &crate::RowSchema::empty_with_control(control)?,
                &[],
                Some(context.routines),
                control,
            )?;
            coerce(
                context,
                plan,
                source.as_deref(),
                &invocation.parameter_types[position],
                control,
            )?
        };
        result.push_produced(control.finish(argument, control.empty_reservation())?)?;
    }
    let (values, memory) = result.finish()?.into_parts();
    Ok(Some(OwnedArguments { values, memory }))
}

fn slot_buffer(count: usize, control: &ProductionControl<'_>) -> Result<Slots, SQLError> {
    let mut slots = ProductionVec::new(*control);
    slots.reserve(count)?;
    for _ in 0..count {
        slots.push_produced(control.finish(None, control.empty_reservation())?)?;
    }
    let (values, memory) = slots.finish()?.into_parts();
    Ok(Slots {
        values,
        _memory: memory,
    })
}

fn coerce(
    context: &RoutineInliningContext<'_>,
    expression: OwnedScalarExpr,
    source: Option<&ColumnType>,
    target: &str,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let target = context
        .types
        .resolve_catalog_column_type_name_with_control(target, control)?;
    let actual = match &*expression {
        ScalarExpr::RetainedLiteral(literal) => literal.ty.as_ref(),
        ScalarExpr::TypedLiteral {
            bound_type: Some(ty),
            ..
        } => Some(ty),
        _ => source,
    };
    if matches!(&*expression, ScalarExpr::Literal(Value::Null))
        && !matches!(&*target, ColumnType::Domain { .. })
    {
        let declaration = target.catalog_name_with_control(control)?;
        let result = OwnedScalarExpr::typed_literal(
            control.finish(Value::Null, control.empty_reservation())?,
            declaration,
            Some(target),
            None,
            None,
            control,
        )?;
        drop(expression);
        Ok(result)
    } else if actual == Some(&target) {
        Ok(expression)
    } else {
        let declaration = target.catalog_name_with_control(control)?;
        drop(target);
        Ok(OwnedScalarExpr::cast(
            expression,
            declaration,
            true,
            control,
        )?)
    }
}
