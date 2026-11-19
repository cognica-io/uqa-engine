//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::stored_input::InputError;
use crate::ast::{
    stored_input::{invalid, Input, Parts},
    FunctionResolutionError, OperatorResolutionError,
};
use uqa_core::{json::JsonToken, memory::Produced};

impl<'a> Input<'a, '_> {
    pub(super) fn resolution_error(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<FunctionResolutionError>, InputError> {
        let (tag, object) = self.tag(token)?;
        if !object {
            return Err(invalid());
        }
        let token = self.next()?;
        let mut parts = Parts::new(self.control);
        let value = match tag.as_str() {
            "UndefinedFunction" => {
                let mut signature = None;
                self.fields(token, &["signature"], 1, |input, _, token| {
                    signature = Some(input.text(token)?);
                    Ok(())
                })?;
                FunctionResolutionError::UndefinedFunction {
                    signature: parts.take(signature.expect("required signature")),
                }
            }
            "Operator" => FunctionResolutionError::Operator(
                parts.take(self.boxed(token, Self::operator_error)?),
            ),
            _ => return Err(invalid()),
        };
        self.close_tag(object)?;
        parts.finish(value)
    }

    fn operator_error(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<OperatorResolutionError>, InputError> {
        let (mut sqlstate, mut message) = (None, None);
        self.fields(token, &["sqlstate", "message"], 3, |input, index, token| {
            if index == 0 {
                sqlstate = Some(input.text(token)?);
            } else {
                message = Some(input.text(token)?);
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let value = OperatorResolutionError {
            sqlstate: parts.take(sqlstate.expect("required SQLSTATE")),
            message: parts.take(message.expect("required message")),
        };
        parts.finish(value)
    }
}
