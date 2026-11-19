//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::stored_input::InputError;
use crate::ast::{
    stored_input::{invalid, number, Input, Parts},
    RoutineInvocationBinding, RoutineVariadicMode,
};
use uqa_core::{json::JsonToken, memory::Produced};

impl<'a> Input<'a, '_> {
    pub(super) fn invocation(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<RoutineInvocationBinding>, InputError> {
        let (mut positions, mut targets, mut sources, mut parameters, mut result) =
            (None, None, None, None, None);
        let mut variadic_mode = RoutineVariadicMode::None;
        self.fields(
            token,
            &[
                "argument_positions",
                "argument_targets",
                "argument_sources",
                "parameter_types",
                "return_type",
                "variadic_mode",
            ],
            0b10_1011,
            |input, index, token| {
                match index {
                    0 => {
                        positions =
                            Some(input.array(token, |input, token| {
                                input.scalar(number::<usize>(token)?)
                            })?);
                    }
                    1 => targets = Some(input.array(token, |input, token| input.text(token))?),
                    2 => {
                        sources = Some(input.array(token, |input, token| {
                            input.maybe(token, |input, token| input.text(token))
                        })?);
                    }
                    3 => parameters = Some(input.array(token, |input, token| input.text(token))?),
                    4 => result = Some(input.maybe(token, |input, token| input.text(token))?),
                    5 => variadic_mode = input.variadic(token)?,
                    _ => unreachable!("declared invocation field"),
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = RoutineInvocationBinding {
            argument_positions: parts.take(positions.expect("required positions")),
            argument_targets: parts.take(targets.expect("required targets")),
            argument_sources: sources.map(|value| parts.take(value)).unwrap_or_default(),
            parameter_types: parts.take(parameters.expect("required parameter types")),
            return_type: result.and_then(|value| parts.take(value)),
            variadic_mode,
        };
        parts.finish(value)
    }

    fn variadic(&mut self, token: JsonToken<'a>) -> Result<RoutineVariadicMode, InputError> {
        let (tag, object) = self.tag(token)?;
        let value = match tag.as_str() {
            "None" => {
                if object && self.next()? != JsonToken::Null {
                    return Err(invalid());
                }
                RoutineVariadicMode::None
            }
            "Expanded" | "Explicit" if object => {
                let mut parameter_index = 0;
                let token = self.next()?;
                self.fields(token, &["parameter_index"], 1, |_, _, token| {
                    parameter_index = number(token)?;
                    Ok(())
                })?;
                if tag.as_str() == "Expanded" {
                    RoutineVariadicMode::Expanded { parameter_index }
                } else {
                    RoutineVariadicMode::Explicit { parameter_index }
                }
            }
            _ => return Err(invalid()),
        };
        self.close_tag(object)?;
        Ok(value)
    }
}
