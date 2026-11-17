//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    FunctionBinding, FunctionResolutionError, Lowering, OperatorResolutionError,
    RoutineInvocationBinding,
};
use crate::plan::scalar::resources::input::{boxed_from, map_from, Work};

impl Lowering<'_> {
    pub(in crate::plan::scalar) async fn binding_from<W: Work>(
        &mut self,
        binding: &FunctionBinding,
        field_projection: Option<(bool, Option<&crate::ColumnType>)>,
        work: &mut W,
    ) -> Result<FunctionBinding, W::Error> {
        self.binding_fields_from(binding, field_projection, true, work)
            .await
    }

    pub(in crate::plan::scalar) async fn binding_fields_from<W: Work>(
        &mut self,
        binding: &FunctionBinding,
        field_projection: Option<(bool, Option<&crate::ColumnType>)>,
        retain_invocation: bool,
        work: &mut W,
    ) -> Result<FunctionBinding, W::Error> {
        self.checkpoint(work).await?;
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
            name: self.text_from(name, work).await?,
            argument_types: map_from!(self, argument_types.iter(), work, |text| self
                .text_from(text, work)
                .await?),
            builtin: *builtin,
            dispatch: *dispatch,
            invocation: match invocation {
                Some(invocation) if retain_invocation => Some(boxed_from!(
                    self,
                    work,
                    RoutineInvocationBinding,
                    self.invocation_from(invocation, work).await?
                )),
                _ => None,
            },
            value_binding: match value_binding {
                Some(binding) => Some(boxed_from!(
                    self,
                    work,
                    crate::ast::FunctionValueBinding,
                    self.value_binding_from(binding, field_projection, work)
                        .await?
                )),
                None => None,
            },
            resolution_error: match resolution_error {
                Some(error) => Some(boxed_from!(
                    self,
                    work,
                    FunctionResolutionError,
                    self.resolution_error_from(error, work).await?
                )),
                None => None,
            },
        })
    }

    async fn value_binding_from<W: Work>(
        &mut self,
        binding: &crate::ast::FunctionValueBinding,
        field_projection: Option<(bool, Option<&crate::ColumnType>)>,
        work: &mut W,
    ) -> Result<crate::ast::FunctionValueBinding, W::Error> {
        use crate::ast::{AssignmentBinding, CompositeFieldBinding, FunctionValueBinding};
        self.checkpoint(work).await?;
        Ok(match binding {
            FunctionValueBinding::Assignment(assignment) => {
                FunctionValueBinding::Assignment(AssignmentBinding {
                    path: map_from!(self, assignment.path.iter(), work, |step| *step),
                    value_argument: assignment.value_argument,
                })
            }
            FunctionValueBinding::CompositeField(field) => {
                FunctionValueBinding::CompositeField(CompositeFieldBinding {
                    collation_oid: field.collation_oid,
                    type_oid: field.type_oid,
                    number: field.number,
                    result_type: self
                        .type_from(Some(&field.result_type), work)
                        .await?
                        .expect("field result type"),
                    dropped: field_projection.map_or(field.dropped, |(dropped, _)| dropped),
                    changed_type: self
                        .type_from(
                            field_projection.map_or(field.changed_type.as_ref(), |(_, ty)| ty),
                            work,
                        )
                        .await?,
                })
            }
        })
    }

    async fn invocation_from<W: Work>(
        &mut self,
        invocation: &RoutineInvocationBinding,
        work: &mut W,
    ) -> Result<RoutineInvocationBinding, W::Error> {
        self.checkpoint(work).await?;
        let RoutineInvocationBinding {
            argument_positions,
            argument_targets,
            argument_sources,
            parameter_types,
            return_type,
            variadic_mode,
        } = invocation;
        Ok(RoutineInvocationBinding {
            argument_positions: map_from!(self, argument_positions.iter(), work, |position| {
                *position
            }),
            argument_targets: map_from!(self, argument_targets.iter(), work, |text| self
                .text_from(text, work)
                .await?),
            argument_sources: map_from!(self, argument_sources.iter(), work, |text| self
                .optional_text_from(text.as_ref(), work)
                .await?),
            parameter_types: map_from!(self, parameter_types.iter(), work, |text| self
                .text_from(text, work)
                .await?),
            return_type: self.optional_text_from(return_type.as_ref(), work).await?,
            variadic_mode: *variadic_mode,
        })
    }

    async fn resolution_error_from<W: Work>(
        &mut self,
        error: &FunctionResolutionError,
        work: &mut W,
    ) -> Result<FunctionResolutionError, W::Error> {
        self.checkpoint(work).await?;
        Ok(match error {
            FunctionResolutionError::UndefinedFunction { signature } => {
                FunctionResolutionError::UndefinedFunction {
                    signature: self.text_from(signature, work).await?,
                }
            }
            FunctionResolutionError::Operator(error) => {
                FunctionResolutionError::Operator(boxed_from!(
                    self,
                    work,
                    OperatorResolutionError,
                    OperatorResolutionError {
                        sqlstate: self.text_from(&error.sqlstate, work).await?,
                        message: self.text_from(&error.message, work).await?,
                    }
                ))
            }
        })
    }
}

#[cfg(test)]
mod tests;
