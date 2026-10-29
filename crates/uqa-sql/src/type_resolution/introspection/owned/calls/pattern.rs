//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pattern and collation syntax select immutable dispatch metadata while preserving lazy operand owners.

use super::{FunctionDispatch, OwnedBinder, OwnedBindingCall, SQLError, ScalarExpr};
use crate::type_resolution::{common::unknown_input, FunctionBindingParts};
use uqa_core::{memory::ProductionVec, Value};

impl OwnedBinder<'_> {
    pub(super) fn bind_collation(
        &self,
        call: OwnedBindingCall,
    ) -> Result<(OwnedBindingCall, bool), SQLError> {
        let Some(FunctionDispatch::Collate { oid }) =
            call.binding().and_then(|binding| binding.dispatch)
        else {
            return Ok((call, false));
        };
        let [argument, ScalarExpr::Literal(Value::Str(name))] = call.arguments() else {
            return Err(SQLError::Internal("invalid COLLATE expression".into()));
        };
        let control = &self.context.control;
        let selected = (|| {
            let ty = self.context.common_context(argument)?;
            crate::catalog::collations::expression::select_with_control(
                name,
                oid,
                ty.as_deref(),
                self.context.resolver,
                control,
            )
        })();
        let Some(oid) = self.context.semantic(selected)? else {
            return Ok((call, true));
        };
        Ok((
            call.set_dispatch(Some(FunctionDispatch::Collate { oid: Some(oid) }), control)?,
            true,
        ))
    }

    pub(super) fn coerce_pattern_call(
        &self,
        mut call: OwnedBindingCall,
    ) -> Result<(OwnedBindingCall, bool), SQLError> {
        let Some(
            dispatch @ (FunctionDispatch::Pattern { .. } | FunctionDispatch::PatternEscape { .. }),
        ) = call.binding().and_then(|binding| binding.dispatch)
        else {
            return Ok((call, false));
        };
        if !(1..=2).contains(&call.arguments().len()) {
            return Err(SQLError::Internal("invalid pattern operands".into()));
        }
        let control = &self.context.control;
        let mut types = ProductionVec::new(*control);
        for argument in call.arguments() {
            let ty = self
                .context
                .semantic(self.context.common_context(argument))?
                .flatten();
            if ty.is_none() && !unknown_input(argument) {
                return Ok((call, true));
            }
            let (ty, memory) = match ty {
                Some(ty) => {
                    let (ty, memory) = ty.into_parts();
                    (Some(ty), memory)
                }
                None => (None, control.empty_reservation()),
            };
            types.push_produced(control.finish(ty, memory)?)?;
        }
        let types = types.finish()?;
        let arguments = [
            types.first().and_then(Option::as_ref),
            types.get(1).and_then(Option::as_ref),
        ];
        let mut helper_types = ProductionVec::new(*control);
        let dispatch = match dispatch {
            FunctionDispatch::PatternEscape { similar } => {
                let Some(selected) =
                    self.context
                        .semantic(crate::type_resolution::pattern::select_escape(
                            similar,
                            &arguments[..types.len()],
                            control,
                        ))?
                else {
                    return Ok((call, true));
                };
                for index in 0..call.arguments().len() {
                    call = call.map_argument(index, control, |argument| {
                        self.operand_cast(argument, &selected)
                    })?;
                    helper_types.push_produced(selected.catalog_name_with_control(control)?)?;
                }
                dispatch
            }
            FunctionDispatch::Pattern { operator, .. } => {
                let Some(selected) =
                    self.context
                        .semantic(crate::type_resolution::pattern::select(
                            operator,
                            &arguments[..types.len()],
                            control,
                        ))?
                else {
                    return Ok((call, true));
                };
                for index in 0..call.arguments().len() {
                    call = call.map_argument(index, control, |argument| {
                        self.operand_cast(argument, selected.argument(index))
                    })?;
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
        let mut binding = FunctionBindingParts::dispatched(dispatch, control)?;
        binding.argument_types = helper_types.finish()?;
        Ok((call.rebind(binding.finish(control)?, control)?, true))
    }
}
