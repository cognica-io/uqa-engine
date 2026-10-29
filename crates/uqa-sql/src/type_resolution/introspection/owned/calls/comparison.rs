//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected comparison signatures wrap independently owned operands in SQL evaluation order.

use super::{ColumnType, FunctionDispatch, OwnedBinder, OwnedBindingCall, SQLError, ScalarExpr};
use crate::{ast::BinaryOp, type_resolution::common::base_type};
use uqa_core::Value;

impl OwnedBinder<'_> {
    pub(in super::super) fn coerce_comparison_call(
        &self,
        call: OwnedBindingCall,
    ) -> Result<OwnedBindingCall, SQLError> {
        let Some(binding) = call.binding().filter(|binding| binding.builtin) else {
            return Ok(call);
        };
        match binding.dispatch {
            Some(FunctionDispatch::IsDistinct | FunctionDispatch::ValueIsDistinct) => {
                self.coerce_argument_pair(call)
            }
            Some(FunctionDispatch::BetweenSymmetric) => {
                let [value, low, high] = call.arguments() else {
                    return Ok(call);
                };
                let low = self
                    .context
                    .semantic(self.context.binary_operand_types(
                        BinaryOp::GreaterEqual,
                        value,
                        low,
                    ))?
                    .flatten();
                let high = self
                    .context
                    .semantic(
                        self.context
                            .binary_operand_types(BinaryOp::LessEqual, value, high),
                    )?
                    .flatten();
                let expression = self.coerce_between(call.into_expression(), low, high)?;
                OwnedBindingCall::new(expression, &self.context.control)
            }
            Some(FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator) => {
                self.coerce_quantified_comparison(call)
            }
            None if binding.name == "nullif" => self.coerce_argument_pair(call),
            _ => Ok(call),
        }
    }

    fn coerce_argument_pair(
        &self,
        mut call: OwnedBindingCall,
    ) -> Result<OwnedBindingCall, SQLError> {
        let [left, right] = call.arguments() else {
            return Ok(call);
        };
        let types = self
            .context
            .semantic(
                self.context
                    .binary_operand_types(BinaryOp::Equal, left, right),
            )?
            .flatten();
        if let Some(types) = types {
            for index in 0..2 {
                call = call.map_argument(index, &self.context.control, |argument| {
                    self.operand_cast(argument, &types[index])
                })?;
            }
        }
        Ok(call)
    }

    pub(in super::super) fn coerce_quantified_comparison(
        &self,
        mut call: OwnedBindingCall,
    ) -> Result<OwnedBindingCall, SQLError> {
        let control = &self.context.control;
        let [value, array, ScalarExpr::Literal(Value::Str(operator))] = call.arguments() else {
            return Ok(call);
        };
        let Some(op) = super::super::super::comparison::comparison_operator(operator) else {
            return Ok(call);
        };
        let value_type = self
            .context
            .semantic(self.context.common_context(value))?
            .flatten();
        let array_type = self
            .context
            .semantic(self.context.common_context(array))?
            .flatten();
        let element_type = match array_type.as_deref().map(base_type) {
            None => None,
            Some(ty) if crate::type_resolution::array_element_type(ty).is_some() => {
                crate::type_resolution::array_element_type(ty)
                    .map(|element| element.clone_with_control(control))
                    .transpose()?
            }
            Some(_) => return Ok(call),
        };
        let element = element_type
            .as_deref()
            .map(|element| element.clone_with_control(control))
            .transpose()?;
        let Some(types) = self
            .context
            .semantic(
                self.context
                    .operator_types(op, value, value_type, array, element_type),
            )?
            .flatten()
        else {
            return Ok(call);
        };
        call = call.map_argument(0, control, |argument| {
            self.operand_cast(argument, &types[0])
        })?;
        let relabel = element
            .as_deref()
            .is_some_and(|element| *element != types[1]);
        if array_type.is_none() || relabel {
            let target =
                ColumnType::array_with_control(types[1].clone_with_control(control)?, control)?;
            call = call.map_argument(1, control, |argument| self.common_cast(argument, &target))?;
        }
        Ok(call)
    }
}
