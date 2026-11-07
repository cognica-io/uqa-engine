//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed callback and option positions keep native wrapper output bounded.

use super::Fields;
use crate::{
    ast::{ColumnType, FunctionValueBinding},
    catalog::foreign_wrapper::{
        ForeignWrapperDefinition, ForeignWrapperFunction, ForeignWrapperHandler,
    },
    SQLError,
};

#[derive(Clone, Copy)]
enum CallbackStep {
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
    Complete,
}
struct Callback<'a> {
    function: &'a ForeignWrapperFunction,
    step: CallbackStep,
}
impl<'a> Callback<'a> {
    fn new(function: &'a ForeignWrapperFunction) -> Self {
        Self {
            function,
            step: CallbackStep::Header,
        }
    }
    fn value(&mut self, fields: &mut Fields<'a>, column_type: &mut Option<&'a ColumnType>) {
        self.step = match self.function.binding.value_binding.as_deref() {
            None => {
                fields.scalar([0]);
                CallbackStep::Complete
            }
            Some(FunctionValueBinding::CompositeField(value)) => {
                fields.scalar([1]);
                fields.composite_field(value);
                *column_type = Some(&value.result_type);
                CallbackStep::ChangedType
            }
            Some(FunctionValueBinding::Assignment(value)) => {
                fields.scalar([2]);
                fields.count(value.path.len());
                fields.count(value.value_argument);
                CallbackStep::Assignment(0)
            }
        };
    }
    fn next(
        &mut self,
        fields: &mut Fields<'a>,
        column_type: &mut Option<&'a ColumnType>,
    ) -> Result<bool, SQLError> {
        loop {
            let binding = &self.function.binding;
            match self.step {
                CallbackStep::Header => {
                    fields.foreign_callback(self.function)?;
                    self.step = CallbackStep::Arguments(0);
                    return Ok(true);
                }
                CallbackStep::Arguments(index) => {
                    if let Some(value) = binding.argument_types.get(index) {
                        fields.text(value);
                        self.step = CallbackStep::Arguments(index + 1);
                        return Ok(true);
                    }
                    self.step = CallbackStep::Invocation;
                }
                CallbackStep::Invocation => {
                    fields.flag(binding.invocation.is_some());
                    if let Some(invocation) = binding.invocation.as_deref() {
                        fields.invocation(invocation);
                        self.step = CallbackStep::Positions(0);
                    } else {
                        self.step = CallbackStep::Value;
                    }
                    return Ok(true);
                }
                CallbackStep::Positions(index) => {
                    let value = binding.invocation.as_deref().expect("invocation positions");
                    if let Some(position) = value.argument_positions.get(index) {
                        fields.count(*position);
                        self.step = CallbackStep::Positions(index + 1);
                        return Ok(true);
                    }
                    self.step = CallbackStep::Targets(0);
                }
                CallbackStep::Targets(index) => {
                    let value = binding.invocation.as_deref().expect("invocation targets");
                    if let Some(target) = value.argument_targets.get(index) {
                        fields.text(target);
                        self.step = CallbackStep::Targets(index + 1);
                        return Ok(true);
                    }
                    self.step = CallbackStep::Sources(0);
                }
                CallbackStep::Sources(index) => {
                    let value = binding.invocation.as_deref().expect("invocation sources");
                    if let Some(source) = value.argument_sources.get(index) {
                        fields.optional_text(source.as_deref());
                        self.step = CallbackStep::Sources(index + 1);
                        return Ok(true);
                    }
                    self.step = CallbackStep::Parameters(0);
                }
                CallbackStep::Parameters(index) => {
                    let value = binding
                        .invocation
                        .as_deref()
                        .expect("invocation parameters");
                    if let Some(parameter) = value.parameter_types.get(index) {
                        fields.text(parameter);
                        self.step = CallbackStep::Parameters(index + 1);
                        return Ok(true);
                    }
                    self.step = CallbackStep::Value;
                }
                CallbackStep::Value => {
                    self.value(fields, column_type);
                    return Ok(true);
                }
                CallbackStep::ChangedType => {
                    let value = binding.composite_field().expect("composite field payload");
                    self.step = CallbackStep::Complete;
                    if let Some(value) = value.changed_type.as_ref() {
                        *column_type = fields.column_type(value);
                        return Ok(true);
                    }
                }
                CallbackStep::Assignment(index) => {
                    let value = binding.assignment().expect("assignment payload");
                    if let Some(step) = value.path.get(index) {
                        fields.assignment_step(step);
                        self.step = CallbackStep::Assignment(index + 1);
                        return Ok(true);
                    }
                    self.step = CallbackStep::Complete;
                }
                CallbackStep::Complete => return Ok(false),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Step {
    Handler,
    Validator,
    ValidatorFunction,
    Options(usize),
    Complete,
}
pub(super) struct Wrapper<'a> {
    value: &'a ForeignWrapperDefinition,
    step: Step,
    callback: Option<Callback<'a>>,
}
impl<'a> Wrapper<'a> {
    pub(super) fn new(value: &'a ForeignWrapperDefinition) -> Self {
        let callback = match &value.handler {
            ForeignWrapperHandler::Function(function) => Some(Callback::new(function)),
            _ => None,
        };
        Self {
            value,
            step: Step::Handler,
            callback,
        }
    }
    pub(super) fn next(
        &mut self,
        fields: &mut Fields<'a>,
        column_type: &mut Option<&'a ColumnType>,
    ) -> Result<bool, SQLError> {
        loop {
            match self.step {
                Step::Handler | Step::ValidatorFunction => {
                    if let Some(callback) = self.callback.as_mut() {
                        if callback.next(fields, column_type)? {
                            return Ok(true);
                        }
                    }
                    self.callback = None;
                    self.step = if matches!(self.step, Step::Handler) {
                        Step::Validator
                    } else {
                        Step::Options(0)
                    };
                }
                Step::Validator => {
                    fields.flag(self.value.validator.is_some());
                    self.callback = self.value.validator.as_ref().map(Callback::new);
                    self.step = Step::ValidatorFunction;
                    return Ok(true);
                }
                Step::Options(index) => {
                    if let Some((key, value)) = self.value.options.get(index) {
                        fields.text(key);
                        fields.text(value);
                        self.step = Step::Options(index + 1);
                        return Ok(true);
                    }
                    self.step = Step::Complete;
                }
                Step::Complete => return Ok(false),
            }
        }
    }
}
