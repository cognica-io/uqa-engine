//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function bindings are copied only for borrowed column ASTs.

use super::{
    resources::{Lowering, Result},
    source::Source,
};
use crate::ast::{
    FunctionBinding, FunctionResolutionError, OperatorResolutionError, RoutineInvocationBinding,
};

impl Lowering<'_> {
    pub(super) fn binding(
        &mut self,
        source: Source<'_, FunctionBinding>,
    ) -> Result<FunctionBinding> {
        self.check()?;
        let binding = match source {
            Source::Owned(binding) => return Ok(binding),
            Source::Borrowed(binding) => binding,
        };
        let FunctionBinding {
            object_id,
            name,
            argument_types,
            builtin,
            dispatch,
            invocation,
            resolution_error,
        } = binding;
        Ok(FunctionBinding {
            object_id: *object_id,
            name: self.copy_text(name)?,
            argument_types: self.map(argument_types.iter(), |this, text| this.copy_text(text))?,
            builtin: *builtin,
            dispatch: *dispatch,
            invocation: invocation
                .as_ref()
                .map(|invocation| self.boxed(|this| this.invocation(invocation)))
                .transpose()?,
            resolution_error: resolution_error
                .as_ref()
                .map(|error| self.resolution_error(error))
                .transpose()?,
        })
    }

    fn invocation(
        &mut self,
        invocation: &RoutineInvocationBinding,
    ) -> Result<RoutineInvocationBinding> {
        let RoutineInvocationBinding {
            argument_positions,
            argument_targets,
            argument_sources,
            parameter_types,
            return_type,
            variadic_mode,
        } = invocation;
        Ok(RoutineInvocationBinding {
            argument_positions: self.map(argument_positions.iter(), |_, position| Ok(*position))?,
            argument_targets: self
                .map(argument_targets.iter(), |this, text| this.copy_text(text))?,
            argument_sources: self.map(argument_sources.iter(), |this, text| {
                text.as_ref().map(|text| this.copy_text(text)).transpose()
            })?,
            parameter_types: self.map(parameter_types.iter(), |this, text| this.copy_text(text))?,
            return_type: return_type
                .as_ref()
                .map(|text| self.copy_text(text))
                .transpose()?,
            variadic_mode: *variadic_mode,
        })
    }

    fn resolution_error(
        &mut self,
        error: &FunctionResolutionError,
    ) -> Result<FunctionResolutionError> {
        Ok(match error {
            FunctionResolutionError::UndefinedFunction { signature } => {
                FunctionResolutionError::UndefinedFunction {
                    signature: self.copy_text(signature)?,
                }
            }
            FunctionResolutionError::Operator(error) => {
                FunctionResolutionError::Operator(self.boxed(|this| {
                    Ok(OperatorResolutionError {
                        sqlstate: this.copy_text(&error.sqlstate)?,
                        message: this.copy_text(&error.message)?,
                    })
                })?)
            }
        })
    }
}
