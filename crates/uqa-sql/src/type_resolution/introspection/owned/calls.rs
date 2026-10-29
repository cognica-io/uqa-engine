//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Call rewrites move argument owners while selected metadata retains its producer reservation.

use super::{ColumnType, OwnedBinder, OwnedScalarExpr, SQLError, ScalarExpr};
use crate::{ast::FunctionDispatch, type_resolution::OwnedBindingCall};
use uqa_core::memory::{Produced, ProductionVec};

mod arrays;
mod comparison;
mod enums;
mod optional;
mod pattern;

impl OwnedBinder<'_> {
    pub(super) fn bind_call(
        &self,
        expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        let call = OwnedBindingCall::new(expression, control)?;
        let mut call = call.map_arguments(control, |_, argument| self.bind(argument))?;
        let (next, selected) = self.bind_collation(call)?;
        call = next;
        if selected {
            return Ok(call.into_expression());
        }
        let (next, selected) = self.coerce_pattern_call(call)?;
        call = next;
        if selected {
            return Ok(call.into_expression());
        }
        for index in 0..call.order_by().len() {
            call = call.map_order(index, control, |expression| self.bind(expression))?;
        }
        call = call.map_filter(control, |expression| self.bind(expression))?;
        if matches!(
            call.binding().and_then(|binding| binding.dispatch),
            Some(
                FunctionDispatch::Concat { .. }
                    | FunctionDispatch::ArrayConcat
                    | FunctionDispatch::ArrayAppend
                    | FunctionDispatch::ArrayPrepend
            )
        ) {
            return Ok(self.coerce_concat_call(call)?.into_expression());
        }
        if let Some(FunctionDispatch::NumericOperator(operator)) =
            call.binding().and_then(|binding| binding.dispatch)
        {
            call = crate::type_resolution::operators::numeric::bind_owned_call(
                operator,
                call,
                &mut |expression| self.context.common_context(expression),
                control,
            )?;
            if call
                .binding()
                .is_some_and(|binding| binding.resolution_error.is_none())
            {
                for index in 0..call.arguments().len() {
                    let Some(name) = call
                        .binding()
                        .and_then(|binding| binding.argument_types.get(index))
                    else {
                        break;
                    };
                    let target = ColumnType::from_sql_name_with_control(name, control)?;
                    call = call.map_argument(index, control, |argument| {
                        self.operand_cast(argument, &target)
                    })?;
                }
            }
            return Ok(call.into_expression());
        }
        if crate::type_resolution::containment::is_operator(call.name()) {
            call = crate::type_resolution::containment::bind_owned_call(
                call,
                &mut |expression| self.context.infer(expression),
                control,
            )?;
        }
        if super::super::helpers::is_common_type_function(call.name()) {
            if let Some(target) = self.context.common_type(call.arguments().iter())? {
                call =
                    call.map_arguments(control, |_, argument| self.common_cast(argument, &target))?;
            }
        }
        call = self.coerce_comparison_call(call)?;
        call = self.coerce_compatible_array_call(call)?;
        call = self.coerce_concat_call(call)?;
        call = self.bind_optional_calls(call)?;
        call = self.bind_enum_call(call)?;
        Ok(self.bind_type_observation(call)?.into_expression())
    }

    fn bind_type_observation(&self, call: OwnedBindingCall) -> Result<OwnedBindingCall, SQLError> {
        if call.arguments().len() != 1
            || !call.binding().is_some_and(|binding| {
                binding.builtin
                    && binding.name == "pg_catalog.pg_typeof"
                    && !matches!(binding.dispatch, Some(FunctionDispatch::TypeOf { .. }))
            })
        {
            return Ok(call);
        }
        let ty = if crate::type_resolution::is_unknown_literal(&call.arguments()[0]) {
            None
        } else {
            self.context
                .semantic(self.context.infer(&call.arguments()[0]))?
                .flatten()
        };
        let oid = ty
            .as_deref()
            .map_or(705, crate::catalog::type_metadata::pg_type_oid);
        call.set_dispatch(
            Some(FunctionDispatch::TypeOf {
                type_oid: u32::try_from(oid).expect("catalog type identity"),
            }),
            &self.context.control,
        )
    }

    fn common_arguments(
        &self,
        arguments: &[ScalarExpr],
    ) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
        let control = &self.context.control;
        let mut types = ProductionVec::new(*control);
        types.reserve(arguments.len())?;
        for argument in arguments {
            let value = self
                .context
                .semantic(self.context.common_context(argument))?
                .flatten();
            let (value, memory) = match value {
                Some(value) => {
                    let (value, memory) = value.into_parts();
                    (Some(value), memory)
                }
                None => (None, control.empty_reservation()),
            };
            types.push_produced(control.finish(value, memory)?)?;
        }
        Ok(types.finish()?)
    }

    fn local_name(&self, name: &str) -> Result<Produced<String>, SQLError> {
        let mut output = uqa_core::memory::ProductionString::new(self.context.control);
        let name = if name
            .get(..11)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("pg_catalog."))
        {
            &name[11..]
        } else {
            name
        };
        output.reserve(name.len())?;
        for character in name.chars() {
            output.push(character.to_ascii_lowercase())?;
        }
        Ok(output.finish()?)
    }
}
