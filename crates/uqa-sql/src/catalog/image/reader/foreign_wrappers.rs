//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed callback and option positions keep native wrapper output bounded.

use super::{functions::Binding, Fields};
use crate::{
    ast::ColumnType,
    catalog::foreign_wrapper::{ForeignWrapperDefinition, ForeignWrapperHandler},
    SQLError,
};

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
    callback: Option<Binding<'a>>,
}
impl<'a> Wrapper<'a> {
    pub(super) fn new(value: &'a ForeignWrapperDefinition) -> Self {
        let callback = match &value.handler {
            ForeignWrapperHandler::Function(function) => Some(Binding::foreign(function)),
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
                    self.callback = self.value.validator.as_ref().map(Binding::foreign);
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
