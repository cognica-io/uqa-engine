//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::stored_input::InputError;
use crate::ast::{
    stored_input::{boolean, invalid, number, optional, Input, Parts},
    AssignmentBinding, AssignmentPathStep, CompositeFieldBinding, FunctionValueBinding,
};
use uqa_core::{json::JsonToken, memory::Produced};

impl<'a> Input<'a, '_> {
    pub(super) fn value_binding(
        &mut self,
        tag: &str,
        token: JsonToken<'a>,
    ) -> Result<Produced<FunctionValueBinding>, InputError> {
        let mut parts = Parts::new(self.control);
        let value = match tag {
            "composite_field" => {
                FunctionValueBinding::CompositeField(parts.take(self.composite_field(token)?))
            }
            "assignment" => FunctionValueBinding::Assignment(parts.take(self.assignment(token)?)),
            _ => return Err(invalid()),
        };
        parts.finish(value)
    }

    fn composite_field(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<CompositeFieldBinding>, InputError> {
        let (mut type_oid, mut number_value, mut collation_oid, mut dropped) = (0, 0, None, false);
        let (mut result_type, mut changed_type) = (None, None);
        self.fields_with_defaults(
            token,
            &[
                "type_oid",
                "number",
                "result_type",
                "collation_oid",
                "dropped",
                "changed_type",
            ],
            0b01_0111,
            0b01_0111,
            |input, index, token| {
                match index {
                    0 => type_oid = number(token)?,
                    1 => number_value = number(token)?,
                    2 => result_type = Some(input.column(token)?),
                    3 => collation_oid = optional(token)?,
                    4 => dropped = boolean(token)?,
                    5 => changed_type = Some(input.maybe(token, Self::column)?),
                    _ => unreachable!("declared composite field"),
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = CompositeFieldBinding {
            type_oid,
            number: number_value,
            collation_oid,
            dropped,
            result_type: parts.take(result_type.expect("required result type")),
            changed_type: changed_type.and_then(|value| parts.take(value)),
        };
        parts.finish(value)
    }

    fn assignment(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<AssignmentBinding>, InputError> {
        let (mut path, mut value_argument) = (None, 0);
        self.fields(
            token,
            &["path", "value_argument"],
            3,
            |input, index, token| {
                if index == 0 {
                    path = Some(input.array(token, |input, token| {
                        let step = input.assignment_step(token)?;
                        input.scalar(step)
                    })?);
                } else {
                    value_argument = number(token)?;
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = AssignmentBinding {
            path: parts.take(path.expect("required path")),
            value_argument,
        };
        parts.finish(value)
    }

    fn assignment_step(&mut self, token: JsonToken<'a>) -> Result<AssignmentPathStep, InputError> {
        let (tag, object) = self.tag(token)?;
        if !object {
            return Err(invalid());
        }
        let token = self.next()?;
        let value = match tag.as_str() {
            "Field" => {
                let (mut type_oid, mut number_value) = (0, 0);
                self.fields(token, &["type_oid", "number"], 3, |_, index, token| {
                    if index == 0 {
                        type_oid = number(token)?;
                    } else {
                        number_value = number(token)?;
                    }
                    Ok(())
                })?;
                AssignmentPathStep::Field {
                    type_oid,
                    number: number_value,
                }
            }
            "Subscripts" => self.assignment_subscripts(token)?,
            _ => return Err(invalid()),
        };
        self.close_tag(object)?;
        Ok(value)
    }

    fn assignment_subscripts(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<AssignmentPathStep, InputError> {
        let (mut first_argument, mut dimensions, mut slice, mut bounds) = (0, 0, false, None);
        self.fields(
            token,
            &["first_argument", "dimensions", "slice", "bounds"],
            15,
            |input, index, token| {
                match index {
                    0 => first_argument = number(token)?,
                    1 => dimensions = number(token)?,
                    2 => slice = boolean(token)?,
                    _ => bounds = Some(input.slice_bounds(token)?),
                }
                Ok(())
            },
        )?;
        Ok(AssignmentPathStep::Subscripts {
            first_argument,
            dimensions,
            slice,
            bounds: bounds.expect("required bounds"),
        })
    }
}
