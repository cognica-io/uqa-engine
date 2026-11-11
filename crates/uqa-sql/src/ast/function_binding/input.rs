//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native stored call metadata is decoded without overload resolution or routine execution.

use super::FunctionBinding;
use crate::{
    ast::{
        stored_input::{boolean, invalid, optional, Input, InputError, Parts},
        FunctionValueBinding,
    },
    catalog::collations::CollationSelection,
    SQLError,
};
use uqa_core::{
    json::JsonToken,
    memory::{Produced, ProductionControl},
};

mod dispatch;
mod errors;
mod invocation;
mod value_binding;

impl FunctionBinding {
    /// Read the existing stored JSON call metadata under the original and invoking resource scope. This preserves selected identities and declarations without binding names or invoking routines.
    pub fn from_stored_json_with_control(
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let mut input = Input::new(text, *control);
        let token = input.next()?;
        let value = input.function_binding(token)?;
        input.end()?;
        Ok(value)
    }
}

const FIELDS: &[&str] = &[
    "object_id",
    "name",
    "argument_types",
    "builtin",
    "dispatch",
    "invocation",
    "resolution_error",
    "input_collation",
];

impl<'a> Input<'a, '_> {
    pub(crate) fn function_binding(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<FunctionBinding>, InputError> {
        let (mut object_id, mut builtin, mut dispatch, mut input_collation) =
            (None, false, None, None);
        let (mut name, mut argument_types, mut invocation, mut resolution_error) =
            (None, None, None, None);
        let mut flattened = None;
        let mut seen = 0_u64;
        self.object(token, |input, key, token| {
            let Some(index) = FIELDS.iter().position(|field| *field == key) else {
                let content = input.content(token)?;
                if flattened.is_none() {
                    match key {
                        "composite_field" => flattened = Some(("composite_field", content)),
                        "assignment" => flattened = Some(("assignment", content)),
                        _ => (),
                    }
                }
                return Ok(());
            };
            let bit = 1 << index;
            if seen & bit != 0 {
                return Err(invalid());
            }
            seen |= bit;
            match index {
                0 if token == JsonToken::Null => (),
                0 => object_id = Some(input.identity(token)?),
                1 => name = Some(input.text(token)?),
                2 => argument_types = Some(input.array(token, |input, token| input.text(token))?),
                3 => builtin = boolean(token)?,
                4 if token == JsonToken::Null => (),
                4 => dispatch = Some(input.dispatch(token)?),
                5 => {
                    invocation = Some(
                        input.maybe(token, |input, token| input.boxed(token, Self::invocation))?,
                    );
                }
                6 => {
                    resolution_error = Some(input.maybe(token, |input, token| {
                        input.boxed(token, Self::resolution_error)
                    })?);
                }
                7 => {
                    input_collation = optional(token)?
                        .map(|oid| CollationSelection::from_oid(oid).ok_or_else(invalid))
                        .transpose()?;
                }
                _ => unreachable!("declared function binding field"),
            }
            Ok(())
        })?;
        if seen & 6 != 6 {
            return Err(invalid());
        }
        let value_binding = self.flattened_binding(flattened)?;
        let mut parts = Parts::new(self.control);
        let value = FunctionBinding {
            object_id,
            builtin,
            dispatch,
            input_collation,
            name: parts.take(name.expect("required function name")),
            argument_types: parts.take(argument_types.expect("required argument types")),
            invocation: invocation.and_then(|value| parts.take(value)),
            resolution_error: resolution_error.and_then(|value| parts.take(value)),
            value_binding: value_binding.map(|value| parts.take(value)),
        };
        parts.finish(value)
    }

    fn flattened_binding(
        &self,
        field: Option<(&str, &str)>,
    ) -> Result<Option<Produced<Box<FunctionValueBinding>>>, InputError> {
        let Some((tag, text)) = field else {
            return Ok(None);
        };
        let mut input = Input::from_content(text, self.control);
        let token = input.next()?;
        let value = input.boxed(token, |input, token| input.value_binding(tag, token));
        // The existing optional flattened enum retains no binding when selected content cannot be typed. Resource failures are distinct from representation errors and remain fallible.
        match value {
            Ok(value) => {
                input.end()?;
                Ok(Some(value))
            }
            Err(InputError::Invalid) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests;
