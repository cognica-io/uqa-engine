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

impl FunctionBinding {
    /// Copy the complete bound identity with the scalar lowerer's existing allocation admission.
    pub fn clone_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>> {
        control.check()?;
        if control.budget().is_none() {
            return Ok(control.finish(self.clone(), None)?);
        }
        let mut copying = Lowering {
            control: Some(super::resources::Control::from_production(*control)),
            owned_children: None,
        };
        let binding = copying.binding(Source::Borrowed(self))?;
        Ok(copying.finish(binding)?.into())
    }
    pub(crate) fn with_composite_projection_with_control(
        &self,
        dropped: bool,
        changed_type: Option<&crate::ColumnType>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>> {
        control.check()?;
        assert!(
            self.composite_field().is_some(),
            "composite projection requires selected field metadata"
        );
        let mut copying = Lowering {
            control: control
                .budget()
                .map(|_| super::resources::Control::from_production(*control)),
            owned_children: None,
        };
        let binding = copying.binding_fields(self, Some((dropped, changed_type)))?;
        Ok(control.finish(binding, copying.control.map(|owner| owner.memory))?)
    }
}

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
        self.binding_fields(binding, None)
    }

    fn binding_fields(
        &mut self,
        binding: &FunctionBinding,
        field_projection: Option<(bool, Option<&crate::ColumnType>)>,
    ) -> Result<FunctionBinding> {
        let FunctionBinding {
            object_id,
            name,
            argument_types,
            builtin,
            dispatch,
            invocation,
            value_binding,
            resolution_error,
            input_collation,
        } = binding;
        Ok(FunctionBinding {
            object_id: *object_id,
            input_collation: *input_collation,
            name: self.copy_text(name)?,
            argument_types: self.map(argument_types.iter(), |this, text| this.copy_text(text))?,
            builtin: *builtin,
            dispatch: *dispatch,
            invocation: invocation
                .as_ref()
                .map(|invocation| self.boxed(|this| this.invocation(invocation)))
                .transpose()?,
            value_binding: value_binding
                .as_ref()
                .map(|binding| {
                    self.boxed(|this| {
                        use crate::ast::FunctionValueBinding;
                        Ok(match binding.as_ref() {
                            FunctionValueBinding::Assignment(assignment) => {
                                FunctionValueBinding::Assignment(crate::ast::AssignmentBinding {
                                    path: this.map(assignment.path.iter(), |_, step| Ok(*step))?,
                                    value_argument: assignment.value_argument,
                                })
                            }
                            FunctionValueBinding::CompositeField(field) => {
                                FunctionValueBinding::CompositeField(
                                    crate::ast::CompositeFieldBinding {
                                        collation_oid: field.collation_oid,
                                        type_oid: field.type_oid,
                                        number: field.number,
                                        result_type: this
                                            .scalar_type_copy(Some(&field.result_type))?
                                            .expect("field result type"),
                                        dropped: field_projection
                                            .map_or(field.dropped, |(dropped, _)| dropped),
                                        changed_type: this.scalar_type_copy(
                                            field_projection
                                                .map_or(field.changed_type.as_ref(), |(_, ty)| ty),
                                        )?,
                                    },
                                )
                            }
                        })
                    })
                })
                .transpose()?,
            resolution_error: resolution_error
                .as_ref()
                .map(|error| self.boxed(|this| this.resolution_error(error)))
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
