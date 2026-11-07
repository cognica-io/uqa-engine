//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native foreign callbacks retain their selected contracts without invocation or name resolution.

use super::Fields;
use crate::{
    ast::{
        AssignmentPathStep, CompositeFieldBinding, FunctionBinding, RoutineInvocationBinding,
        RoutineVariadicMode,
    },
    catalog::foreign_wrapper::{
        ForeignWrapperDefinition, ForeignWrapperFunction, ForeignWrapperHandler,
        ForeignWrapperReference, NativeForeignWrapper,
    },
    SQLError,
};

impl<'a> Fields<'a> {
    pub fn foreign_wrapper(&mut self, key: &'a str, value: &'a ForeignWrapperDefinition) {
        let ForeignWrapperDefinition {
            name,
            identity,
            owner,
            handler,
            validator: _,
            options,
        } = value;
        let ForeignWrapperReference { oid, object_id } = identity;
        self.text(key);
        self.text(name);
        self.scalar(oid.to_le_bytes());
        self.scalar(*object_id);
        self.identity(*owner);
        self.count(options.len());
        match handler {
            ForeignWrapperHandler::None => self.scalar([0]),
            ForeignWrapperHandler::Native(native) => {
                self.scalar([1]);
                self.scalar([match native {
                    NativeForeignWrapper::Memory => 0,
                    NativeForeignWrapper::DuckDB => 1,
                    NativeForeignWrapper::Arrow => 2,
                }]);
            }
            ForeignWrapperHandler::Function(_) => self.scalar([2]),
        }
    }
    pub fn foreign_callback(&mut self, value: &'a ForeignWrapperFunction) -> Result<(), SQLError> {
        if !value.has_valid_identity() {
            return Err(super::super::invalid("foreign callback identity"));
        }
        let ForeignWrapperFunction { oid, binding } = value;
        let FunctionBinding {
            object_id,
            name,
            argument_types,
            builtin,
            dispatch: _,
            invocation: _,
            value_binding: _,
            resolution_error: _,
            input_collation,
        } = binding;
        self.scalar(oid.to_le_bytes());
        self.flag(object_id.is_some());
        if let Some(id) = object_id {
            self.scalar(*id);
        }
        self.text(name);
        self.flag(*builtin);
        self.optional_u32(input_collation.map(|value| value.oid()));
        self.count(argument_types.len());
        Ok(())
    }
    pub fn invocation(&mut self, value: &'a RoutineInvocationBinding) {
        let RoutineInvocationBinding {
            argument_positions,
            argument_targets,
            argument_sources,
            parameter_types,
            return_type,
            variadic_mode,
        } = value;
        self.count(argument_positions.len());
        self.count(argument_targets.len());
        self.count(argument_sources.len());
        self.count(parameter_types.len());
        self.optional_text(return_type.as_deref());
        match variadic_mode {
            RoutineVariadicMode::None => self.scalar([0]),
            RoutineVariadicMode::Expanded { parameter_index } => {
                self.scalar([1]);
                self.count(*parameter_index);
            }
            RoutineVariadicMode::Explicit { parameter_index } => {
                self.scalar([2]);
                self.count(*parameter_index);
            }
        }
    }
    pub fn composite_field(&mut self, value: &CompositeFieldBinding) {
        let CompositeFieldBinding {
            type_oid,
            number,
            result_type: _,
            collation_oid,
            dropped,
            changed_type,
        } = value;
        self.scalar(type_oid.to_le_bytes());
        self.scalar(number.to_le_bytes());
        self.optional_u32(*collation_oid);
        self.flag(*dropped);
        self.flag(changed_type.is_some());
    }
    pub fn assignment_step(&mut self, value: &AssignmentPathStep) {
        match value {
            AssignmentPathStep::Field { type_oid, number } => {
                self.scalar([0]);
                self.scalar(type_oid.to_le_bytes());
                self.scalar(number.to_le_bytes());
            }
            AssignmentPathStep::Subscripts {
                first_argument,
                dimensions,
                slice,
                bounds,
            } => {
                self.scalar([1]);
                self.count(*first_argument);
                self.count(*dimensions);
                self.flag(*slice);
                self.scalar(bounds.stored_masks());
            }
        }
    }
}
