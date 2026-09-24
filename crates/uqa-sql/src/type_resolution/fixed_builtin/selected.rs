//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constructors for a previously selected fixed call retain their input tree and every new output allocation together.

use super::{default_argument, named_argument_value, named_argument_value_owned, registry};
use crate::{ast::FunctionBinding, ColumnType, SQLError, ScalarExpr};
use uqa_core::{
    memory::{BudgetedVec, MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value,
};

#[derive(Debug)]
pub(super) struct SelectedCall {
    pub(super) binding: FunctionBinding,
    pub(super) arguments: Vec<ScalarExpr>,
}

impl SelectedCall {
    pub(super) fn take(binding: &mut FunctionBinding, arguments: &mut Vec<ScalarExpr>) -> Self {
        Self {
            binding: std::mem::replace(
                binding,
                FunctionBinding {
                    object_id: None,
                    name: String::new(),
                    argument_types: Vec::new(),
                    builtin: true,
                    dispatch: None,
                    invocation: None,
                    resolution_error: None,
                },
            ),
            arguments: std::mem::take(arguments),
        }
    }
}

// Standalone wrappers destroy their call before releasing its lease.
#[cfg(test)]
struct CallOwner {
    call: SelectedCall,
    memory: Option<MemoryReservation>,
}

struct Construction<'a, 'b> {
    call: &'b mut SelectedCall,
    memory: &'b mut Option<MemoryReservation>,
    control: ProductionControl<'a>,
}

impl Construction<'_, '_> {
    fn retain<T>(&mut self, value: Produced<T>) -> T {
        let (value, memory) = value.into_parts();
        *self.memory = self.control.combine(self.memory.take(), memory);
        value
    }

    fn cast(&mut self, argument: ScalarExpr, ty: &ColumnType) -> Result<ScalarExpr, SQLError> {
        let name = ty.sql_name_with_control(&self.control)?;
        let memory = self.control.reserve(size_of::<ScalarExpr>())?;
        *self.memory = self.control.combine(self.memory.take(), memory);
        let ty = self.retain(name);
        Ok(ScalarExpr::Cast {
            expr: Box::new(argument),
            ty,
        })
    }
}

/// Argument inference belongs to the caller; original types are indexed by supplied position, while effective types retain unknown-literal overload matching. Reordering does not infer the same expression again.
#[cfg(test)]
pub(super) fn bind_call_with_control(
    call: Produced<SelectedCall>,
    names: &[Option<String>],
    original_types: &[Option<ColumnType>],
    effective_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<(bool, Produced<SelectedCall>), SQLError> {
    let call = super::super::call::check_owner(call, control)?;
    let (call, memory) = call.into_parts();
    let mut owner = CallOwner { call, memory };
    let matched = bind_call_in_place_with_control(
        &mut owner.call,
        &mut owner.memory,
        names,
        original_types,
        effective_types,
        control,
    )?;
    Ok((matched, control.finish(owner.call, owner.memory)?))
}

/// Borrow the enclosing expression lease so sibling allocations remain retained if this constructor fails after moving supplied arguments.
pub(super) fn bind_call_in_place_with_control(
    call: &mut SelectedCall,
    memory: &mut Option<MemoryReservation>,
    names: &[Option<String>],
    original_types: &[Option<ColumnType>],
    effective_types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    super::super::call::check_memory(memory.as_ref(), control)?;
    let mut construction = Construction {
        call,
        memory,
        control: *control,
    };
    let Some(signature) = registry::bound_signature(&construction.call.binding, control)? else {
        return Ok(false);
    };
    let Some(matched) = super::super::overload_resolution::match_signature_with_control(
        signature,
        names,
        effective_types,
        control,
    )?
    else {
        return Ok(false);
    };
    if construction.call.arguments.len() != original_types.len()
        || !valid_positions(
            &matched.argument_positions,
            construction.call.arguments.len(),
            signature.argument_types.len(),
            &construction.call.binding.name,
        )
    {
        return Ok(false);
    }
    let mut arguments = Arguments::new(signature.argument_types.len(), control)?;
    for (position, declared) in signature.argument_types.iter().enumerate() {
        control.check()?;
        let supplied = matched
            .argument_positions
            .iter()
            .position(|&slot| slot == position);
        let requires_cast = match supplied {
            Some(index) => requires_cast(
                named_argument_value(&construction.call.arguments[index]),
                original_types[index].as_ref(),
                declared,
                control,
            )?,
            None => false,
        };
        let argument = supplied.map_or_else(
            || {
                default_argument(&construction.call.binding.name, position)
                    .expect("missing fixed arguments were prevalidated")
            },
            |index| {
                named_argument_value_owned(std::mem::replace(
                    &mut construction.call.arguments[index],
                    ScalarExpr::Literal(Value::Null),
                ))
            },
        );
        let argument = if requires_cast {
            construction.cast(argument, declared)?
        } else {
            argument
        };
        arguments.push(argument)?;
    }
    let (arguments, memory) = arguments.into_parts();
    *construction.memory = control.combine(construction.memory.take(), memory);
    construction.call.arguments = arguments;
    let (name, _) = registry::lookup(&construction.call.binding.name)
        .expect("selected registry descriptor exists");
    let name = control.format(format_args!("pg_catalog.{name}"))?;
    let mut types = ProductionVec::new(*control);
    types.reserve(signature.argument_types.len())?;
    for ty in signature.argument_types {
        types.push_produced(ty.sql_name_with_control(control)?)?;
    }
    let types = types.finish()?;
    construction.call.binding.name = construction.retain(name);
    construction.call.binding.argument_types = construction.retain(types);
    construction.call.binding.object_id = None;
    construction.call.binding.builtin = true;
    construction.call.binding.invocation = None;
    construction.call.binding.resolution_error = None;
    construction.call.binding.dispatch = super::runtime_dispatch(&construction.call.binding);
    control.check()?;
    Ok(true)
}

fn valid_positions(positions: &[usize], count: usize, parameters: usize, name: &str) -> bool {
    positions.len() == count
        && positions.iter().enumerate().all(|(index, position)| {
            *position < parameters && !positions[..index].contains(position)
        })
        && (0..parameters).all(|position| {
            positions.contains(&position) || default_argument(name, position).is_some()
        })
}

fn requires_cast(
    argument: &ScalarExpr,
    actual: Option<&ColumnType>,
    declared: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if matches!(
        argument,
        ScalarExpr::Literal(Value::Str(_) | Value::Null) | ScalarExpr::Param(_)
    ) {
        return Ok(true);
    }
    let Some(actual) = actual else {
        return Ok(true);
    };
    let actual = super::super::overload_resolution::canonical_column_type_name_with_control(
        super::base_type(actual),
        control,
    )?;
    let declared = declared.sql_name_with_control(control)?;
    let declared = super::super::overload_resolution::canonical_routine_type_name_with_control(
        &declared, control,
    )?;
    Ok(*actual != *declared)
}

enum Arguments {
    Ordinary(Vec<ScalarExpr>),
    Controlled(BudgetedVec<ScalarExpr>),
}

impl Arguments {
    fn new(count: usize, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        control.check()?;
        Ok(match control.budget() {
            Some(budget) => {
                let mut values = BudgetedVec::new(budget);
                values.reserve(count)?;
                Self::Controlled(values)
            }
            None => Self::Ordinary(Vec::with_capacity(count)),
        })
    }

    fn push(&mut self, value: ScalarExpr) -> Result<(), SQLError> {
        match self {
            Self::Ordinary(values) => values.push(value),
            Self::Controlled(values) => values.push(value)?,
        }
        Ok(())
    }

    fn into_parts(self) -> (Vec<ScalarExpr>, Option<MemoryReservation>) {
        match self {
            Self::Ordinary(values) => (values, None),
            Self::Controlled(values) => {
                let (values, memory) = values.into_parts();
                (values, Some(memory))
            }
        }
    }
}

#[cfg(test)]
mod tests;
