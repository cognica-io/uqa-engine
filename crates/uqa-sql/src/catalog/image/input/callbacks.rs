//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native callback reconstruction preserves optional invocation and typed value metadata without resolving a routine.

use super::{invalid, Fields};
use crate::{
    ast::{
        ArraySliceBounds, AssignmentBinding, AssignmentPathStep, CompositeFieldBinding,
        FunctionBinding, FunctionValueBinding, RoutineInvocationBinding, RoutineVariadicMode,
    },
    catalog::{collations::CollationSelection, foreign_wrapper::ForeignWrapperFunction},
    SQLError,
};
use uqa_core::memory::Budgeted;

impl Fields<'_> {
    async fn callback_invocation(
        &mut self,
    ) -> Result<Budgeted<Option<Box<RoutineInvocationBinding>>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        if !self.flag().await? {
            return Ok(Budgeted::new(None, memory));
        }
        memory.grow(size_of::<RoutineInvocationBinding>())?;
        let positions = self.binary.count().await?;
        let targets = self.binary.count().await?;
        let sources = self.binary.count().await?;
        let parameters = self.binary.count().await?;
        let (return_type, reservation) = self.optional_text().await?.into_parts();
        memory.absorb(reservation);
        let variadic_mode = match self.binary.array::<1>().await?[0] {
            0 => RoutineVariadicMode::None,
            1 => RoutineVariadicMode::Expanded {
                parameter_index: self.binary.count().await?,
            },
            2 => RoutineVariadicMode::Explicit {
                parameter_index: self.binary.count().await?,
            },
            _ => return Err(invalid("routine variadic mode")),
        };
        let (argument_positions, reservation) =
            self.image_values::<usize>(positions).await?.into_parts();
        memory.absorb(reservation);
        let (argument_targets, reservation) =
            self.image_values::<String>(targets).await?.into_parts();
        memory.absorb(reservation);
        let (argument_sources, reservation) = self
            .image_values::<Option<String>>(sources)
            .await?
            .into_parts();
        memory.absorb(reservation);
        let (parameter_types, reservation) =
            self.image_values::<String>(parameters).await?.into_parts();
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
    async fn callback_value(
        &mut self,
    ) -> Result<Budgeted<Option<Box<FunctionValueBinding>>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let tag = self.binary.array::<1>().await?[0];
        if tag == 0 {
            return Ok(Budgeted::new(None, memory));
        }
        memory.grow(size_of::<FunctionValueBinding>())?;
        let value = match tag {
            1 => {
                let type_oid = u32::from_le_bytes(self.binary.array().await?);
                let number = i16::from_le_bytes(self.binary.array().await?);
                let collation_oid = self.optional_number().await?;
                let dropped = self.flag().await?;
                let changed = self.flag().await?;
                let (result_type, reservation) = self.column_type().await?.into_parts();
                memory.absorb(reservation);
                let changed_type = if changed {
                    let (value, reservation) = self.column_type().await?.into_parts();
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
                let count = self.binary.count().await?;
                let value_argument = self.binary.count().await?;
                let (path, reservation) = self
                    .image_values::<AssignmentPathStep>(count)
                    .await?
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
    pub(super) async fn function_binding_fields(
        &mut self,
    ) -> Result<Budgeted<FunctionBinding>, SQLError> {
        let object_id = if self.flag().await? {
            Some(self.binary.array().await?)
        } else {
            None
        };
        let mut memory = self.binary.memory.empty_reservation();
        let (name, reservation) = self.binary.text().await?.into_parts();
        memory.absorb(reservation);
        let builtin = self.flag().await?;
        let input_collation = self
            .optional_number()
            .await?
            .map(|oid| {
                CollationSelection::from_oid(oid)
                    .ok_or_else(|| invalid("selected callback collation"))
            })
            .transpose()?;
        let count = self.binary.count().await?;
        let (argument_types, reservation) = self.image_values::<String>(count).await?.into_parts();
        memory.absorb(reservation);
        let (invocation, reservation) = self.callback_invocation().await?.into_parts();
        memory.absorb(reservation);
        let (value_binding, reservation) = self.callback_value().await?.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(
            FunctionBinding {
                object_id,
                name,
                argument_types,
                builtin,
                input_collation,
                invocation,
                value_binding,
                dispatch: None,
                resolution_error: None,
            },
            memory,
        ))
    }
    pub(super) async fn foreign_callback(
        &mut self,
    ) -> Result<Budgeted<ForeignWrapperFunction>, SQLError> {
        let oid = self.catalog_oid().await?;
        let (binding, memory) = self.function_binding_fields().await?.into_parts();
        let value = ForeignWrapperFunction { oid, binding };
        if !value.has_valid_identity() {
            return Err(invalid("foreign callback identity"));
        }
        Ok(Budgeted::new(value, memory))
    }
}

impl super::collections::ImageValue for AssignmentPathStep {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let step = match input.binary.array::<1>().await?[0] {
            0 => AssignmentPathStep::Field {
                type_oid: u32::from_le_bytes(input.binary.array().await?),
                number: i16::from_le_bytes(input.binary.array().await?),
            },
            1 => AssignmentPathStep::Subscripts {
                first_argument: input.binary.count().await?,
                dimensions: input.binary.count().await?,
                slice: input.flag().await?,
                bounds: ArraySliceBounds::from_stored_masks(input.binary.array().await?),
            },
            _ => return Err(invalid("assignment path tag")),
        };
        Ok(Budgeted::new(step, input.binary.memory.empty_reservation()))
    }
}
