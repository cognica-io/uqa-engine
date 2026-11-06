//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native callback reconstruction preserves optional invocation and typed value metadata without resolving a routine.

use super::{invalid, Input};
use crate::{
    ast::{
        ArraySliceBounds, AssignmentBinding, AssignmentPathStep, CompositeFieldBinding,
        FunctionBinding, FunctionValueBinding, RoutineInvocationBinding, RoutineVariadicMode,
    },
    catalog::{collations::CollationSelection, foreign_wrapper::ForeignWrapperFunction},
    SQLError,
};
use uqa_core::memory::{Budgeted, BudgetedVec};

impl Input<'_> {
    pub(super) fn image_values<T>(
        &mut self,
        count: usize,
        mut read: impl FnMut(&mut Self) -> Result<Budgeted<T>, SQLError>,
    ) -> Result<Budgeted<Vec<T>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let mut values = BudgetedVec::new(self.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let (value, reservation) = read(self)?.into_parts();
            memory.absorb(reservation);
            values.push(value)?;
        }
        let (values, reservation) = values.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(values, memory))
    }
    fn callback_invocation(
        &mut self,
    ) -> Result<Budgeted<Option<Box<RoutineInvocationBinding>>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        if !self.flag()? {
            return Ok(Budgeted::new(None, memory));
        }
        memory.grow(size_of::<RoutineInvocationBinding>())?;
        let positions = self.binary.count()?;
        let targets = self.binary.count()?;
        let sources = self.binary.count()?;
        let parameters = self.binary.count()?;
        let (return_type, reservation) = self.optional_text()?.into_parts();
        memory.absorb(reservation);
        let variadic_mode = match self.binary.array::<1>()?[0] {
            0 => RoutineVariadicMode::None,
            1 => RoutineVariadicMode::Expanded {
                parameter_index: self.binary.count()?,
            },
            2 => RoutineVariadicMode::Explicit {
                parameter_index: self.binary.count()?,
            },
            _ => return Err(invalid("routine variadic mode")),
        };
        let (argument_positions, reservation) = self
            .image_values(positions, |input| {
                Ok(Budgeted::new(
                    input.binary.count()?,
                    input.binary.memory.empty_reservation(),
                ))
            })?
            .into_parts();
        memory.absorb(reservation);
        let (argument_targets, reservation) = self
            .image_values(targets, |input| Ok(input.binary.text()?))?
            .into_parts();
        memory.absorb(reservation);
        let (argument_sources, reservation) = self
            .image_values(sources, Self::optional_text)?
            .into_parts();
        memory.absorb(reservation);
        let (parameter_types, reservation) = self
            .image_values(parameters, |input| Ok(input.binary.text()?))?
            .into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(
            Some(Box::new(RoutineInvocationBinding {
                argument_positions,
                argument_targets,
                argument_sources,
                parameter_types,
                return_type,
                variadic_mode,
            })),
            memory,
        ))
    }
    fn callback_value(&mut self) -> Result<Budgeted<Option<Box<FunctionValueBinding>>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let tag = self.binary.array::<1>()?[0];
        if tag == 0 {
            return Ok(Budgeted::new(None, memory));
        }
        memory.grow(size_of::<FunctionValueBinding>())?;
        let value = match tag {
            1 => {
                let type_oid = u32::from_le_bytes(self.binary.array()?);
                let number = i16::from_le_bytes(self.binary.array()?);
                let collation_oid = self.optional_number()?;
                let dropped = self.flag()?;
                let changed = self.flag()?;
                let (result_type, reservation) = self.column_type()?.into_parts();
                memory.absorb(reservation);
                let changed_type = if changed {
                    let (value, reservation) = self.column_type()?.into_parts();
                    memory.absorb(reservation);
                    Some(value)
                } else {
                    None
                };
                FunctionValueBinding::CompositeField(CompositeFieldBinding {
                    type_oid,
                    number,
                    result_type,
                    collation_oid,
                    dropped,
                    changed_type,
                })
            }
            2 => {
                let count = self.binary.count()?;
                let value_argument = self.binary.count()?;
                let (path, reservation) = self
                    .image_values(count, |input| {
                        let step = match input.binary.array::<1>()?[0] {
                            0 => AssignmentPathStep::Field {
                                type_oid: u32::from_le_bytes(input.binary.array()?),
                                number: i16::from_le_bytes(input.binary.array()?),
                            },
                            1 => AssignmentPathStep::Subscripts {
                                first_argument: input.binary.count()?,
                                dimensions: input.binary.count()?,
                                slice: input.flag()?,
                                bounds: ArraySliceBounds::from_stored_masks(input.binary.array()?),
                            },
                            _ => return Err(invalid("assignment path tag")),
                        };
                        Ok(Budgeted::new(step, input.binary.memory.empty_reservation()))
                    })?
                    .into_parts();
                memory.absorb(reservation);
                FunctionValueBinding::Assignment(AssignmentBinding {
                    path,
                    value_argument,
                })
            }
            _ => return Err(invalid("function value binding tag")),
        };
        Ok(Budgeted::new(Some(Box::new(value)), memory))
    }
    pub(super) fn foreign_callback(
        &mut self,
    ) -> Result<Budgeted<ForeignWrapperFunction>, SQLError> {
        let oid = self.catalog_oid()?;
        let object_id = if self.flag()? {
            Some(self.binary.array()?)
        } else {
            None
        };
        let (name, mut memory) = self.binary.text()?.into_parts();
        let builtin = self.flag()?;
        let input_collation = self
            .optional_number()?
            .map(|oid| {
                CollationSelection::from_oid(oid)
                    .ok_or_else(|| invalid("selected callback collation"))
            })
            .transpose()?;
        let count = self.binary.count()?;
        let (argument_types, reservation) = self
            .image_values(count, |input| Ok(input.binary.text()?))?
            .into_parts();
        memory.absorb(reservation);
        let (invocation, reservation) = self.callback_invocation()?.into_parts();
        memory.absorb(reservation);
        let (value_binding, reservation) = self.callback_value()?.into_parts();
        memory.absorb(reservation);
        let value = ForeignWrapperFunction {
            oid,
            binding: FunctionBinding {
                object_id,
                name,
                argument_types,
                builtin,
                dispatch: None,
                invocation,
                value_binding,
                resolution_error: None,
                input_collation,
            },
        };
        if !value.has_valid_identity() {
            return Err(invalid("foreign callback identity"));
        }
        Ok(Budgeted::new(value, memory))
    }
}
