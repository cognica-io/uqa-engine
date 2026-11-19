//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed native function metadata preserves original dispatch and invocation contracts.

use super::Fields;
use crate::{
    ast::{ColumnType, FunctionBinding, FunctionValueBinding},
    catalog::foreign_wrapper::ForeignWrapperFunction,
    SQLError,
};

#[derive(Clone, Copy)]
enum BindingStep {
    Header,
    Arguments(usize),
    Invocation,
    Positions(usize),
    Targets(usize),
    Sources(usize),
    Parameters(usize),
    Value,
    ChangedType,
    Assignment(usize),
    Dispatch,
    Error,
    Complete,
}
pub(super) struct Binding<'a> {
    value: &'a FunctionBinding,
    callback: Option<&'a ForeignWrapperFunction>,
    step: BindingStep,
}
impl<'a> Binding<'a> {
    pub(super) fn new(binding: &'a FunctionBinding) -> Self {
        Self {
            value: binding,
            callback: None,
            step: BindingStep::Header,
        }
    }
    pub(super) fn foreign(function: &'a ForeignWrapperFunction) -> Self {
        Self {
            value: &function.binding,
            callback: Some(function),
            step: BindingStep::Header,
        }
    }
    fn value_binding(&mut self, fields: &mut Fields<'a>, column_type: &mut Option<&'a ColumnType>) {
        self.step = match self.value.value_binding.as_deref() {
            None => {
                fields.scalar([0]);
                BindingStep::Dispatch
            }
            Some(FunctionValueBinding::CompositeField(value)) => {
                fields.scalar([1]);
                fields.composite_field(value);
                *column_type = Some(&value.result_type);
                BindingStep::ChangedType
            }
            Some(FunctionValueBinding::Assignment(value)) => {
                fields.scalar([2]);
                fields.count(value.path.len());
                fields.count(value.value_argument);
                BindingStep::Assignment(0)
            }
        };
    }
    fn header(&mut self, fields: &mut Fields<'a>) -> Result<(), SQLError> {
        if let Some(function) = self.callback {
            fields.foreign_callback(function)?;
        } else {
            fields.function_binding(self.value);
        }
        self.step = BindingStep::Arguments(0);
        Ok(())
    }
    fn invocation(&mut self, fields: &mut Fields<'a>) {
        fields.flag(self.value.invocation.is_some());
        if let Some(invocation) = self.value.invocation.as_deref() {
            fields.invocation(invocation);
            self.step = BindingStep::Positions(0);
        } else {
            self.step = BindingStep::Value;
        }
    }
    pub(super) fn next(
        &mut self,
        fields: &mut Fields<'a>,
        column_type: &mut Option<&'a ColumnType>,
    ) -> Result<bool, SQLError> {
        loop {
            let binding = self.value;
            match self.step {
                BindingStep::Header => {
                    self.header(fields)?;
                    return Ok(true);
                }
                BindingStep::Arguments(index) => {
                    if let Some(value) = binding.argument_types.get(index) {
                        fields.text(value);
                        self.step = BindingStep::Arguments(index + 1);
                        return Ok(true);
                    }
                    self.step = BindingStep::Invocation;
                }
                BindingStep::Invocation => {
                    self.invocation(fields);
                    return Ok(true);
                }
                BindingStep::Positions(index) => {
                    let value = binding.invocation.as_deref().expect("invocation positions");
                    if let Some(position) = value.argument_positions.get(index) {
                        fields.count(*position);
                        self.step = BindingStep::Positions(index + 1);
                        return Ok(true);
                    }
                    self.step = BindingStep::Targets(0);
                }
                BindingStep::Targets(index) => {
                    let value = binding.invocation.as_deref().expect("invocation targets");
                    if let Some(target) = value.argument_targets.get(index) {
                        fields.text(target);
                        self.step = BindingStep::Targets(index + 1);
                        return Ok(true);
                    }
                    self.step = BindingStep::Sources(0);
                }
                BindingStep::Sources(index) => {
                    let value = binding.invocation.as_deref().expect("invocation sources");
                    if let Some(source) = value.argument_sources.get(index) {
                        fields.optional_text(source.as_deref());
                        self.step = BindingStep::Sources(index + 1);
                        return Ok(true);
                    }
                    self.step = BindingStep::Parameters(0);
                }
                BindingStep::Parameters(index) => {
                    let value = binding
                        .invocation
                        .as_deref()
                        .expect("invocation parameters");
                    if let Some(parameter) = value.parameter_types.get(index) {
                        fields.text(parameter);
                        self.step = BindingStep::Parameters(index + 1);
                        return Ok(true);
                    }
                    self.step = BindingStep::Value;
                }
                BindingStep::Value => {
                    self.value_binding(fields, column_type);
                    return Ok(true);
                }
                BindingStep::ChangedType => {
                    let value = binding.composite_field().expect("composite field payload");
                    self.step = BindingStep::Dispatch;
                    if let Some(value) = value.changed_type.as_ref() {
                        *column_type = fields.column_type(value);
                        return Ok(true);
                    }
                }
                BindingStep::Assignment(index) => {
                    let value = binding.assignment().expect("assignment payload");
                    if let Some(step) = value.path.get(index) {
                        fields.assignment_step(step);
                        self.step = BindingStep::Assignment(index + 1);
                        return Ok(true);
                    }
                    self.step = BindingStep::Dispatch;
                }
                BindingStep::Dispatch => {
                    if self.callback.is_some() {
                        self.step = BindingStep::Complete;
                    } else {
                        fields.function_dispatch(binding.dispatch);
                        self.step = BindingStep::Error;
                        return Ok(true);
                    }
                }
                BindingStep::Error => {
                    fields.function_resolution_error(binding.resolution_error.as_deref());
                    self.step = BindingStep::Complete;
                    return Ok(true);
                }
                BindingStep::Complete => return Ok(false),
            }
        }
    }
}
