//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind native pattern operands without consulting the SQL routine namespace.

use super::{Binder, BindingCall, FunctionBinding, SQLError};
use crate::ast::FunctionDispatch;

impl Binder<'_, '_> {
    pub(super) fn coerce_pattern_call(&mut self, call: &mut BindingCall) -> Result<bool, SQLError> {
        let Some(
            dispatch @ (FunctionDispatch::Pattern { .. } | FunctionDispatch::PatternEscape { .. }),
        ) = call.binding.as_ref().and_then(|binding| binding.dispatch)
        else {
            return Ok(false);
        };
        if !(1..=2).contains(&call.arguments.len()) {
            return Err(SQLError::Internal("invalid pattern operands".into()));
        }
        let mut types = uqa_core::memory::ProductionVec::new(self.control);
        for argument in &call.arguments {
            let ty = self.semantic(self.common_context(argument))?.flatten();
            if ty.is_none() && !super::comparison::unknown_input(argument) {
                return Ok(true);
            }
            let (ty, memory) = ty.map_or_else(
                || (None, self.control.empty_reservation()),
                |ty| {
                    let (ty, memory) = ty.into_parts();
                    (Some(ty), memory)
                },
            );
            types.push_produced(self.control.finish(ty, memory)?)?;
        }
        let types = types.finish()?;
        let arguments = [
            types.first().and_then(Option::as_ref),
            types.get(1).and_then(Option::as_ref),
            types.get(2).and_then(Option::as_ref),
        ];
        let mut helper_types = uqa_core::memory::ProductionVec::new(self.control);
        let dispatch = match dispatch {
            FunctionDispatch::PatternEscape { similar } => {
                let Some(selected) =
                    self.semantic(crate::type_resolution::pattern::select_escape(
                        similar,
                        &arguments[..types.len()],
                        &self.control,
                    ))?
                else {
                    return Ok(true);
                };
                for argument in &mut call.arguments {
                    self.operand_cast(argument, &selected)?;
                    helper_types
                        .push_produced(selected.catalog_name_with_control(&self.control)?)?;
                }
                dispatch
            }
            FunctionDispatch::Pattern { operator, .. } => {
                let Some(selected) = self.semantic(crate::type_resolution::pattern::select(
                    operator,
                    &arguments[..types.len()],
                    &self.control,
                ))?
                else {
                    return Ok(true);
                };
                for (position, argument) in call.arguments.iter_mut().enumerate() {
                    self.operand_cast(argument, selected.argument(position))?;
                }
                FunctionDispatch::Pattern {
                    operator,
                    oid: Some(
                        std::num::NonZeroU32::new(
                            u32::try_from(selected.identity.oid)
                                .expect("catalog operator identity"),
                        )
                        .expect("selected operator identity is nonzero"),
                    ),
                }
            }
            _ => unreachable!("pattern call dispatch"),
        };
        let binding = FunctionBinding::dispatched_with_control(dispatch, &self.control)?;
        let mut binding = self.retain(binding);
        binding.argument_types = self.retain(helper_types.finish()?);
        call.binding = Some(binding);
        Ok(true)
    }
}
