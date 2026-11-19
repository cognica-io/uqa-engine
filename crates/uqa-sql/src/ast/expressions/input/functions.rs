//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Child, Expr, Input, InputError, Parts, Slot};
use crate::ast::{
    stored_input::{boolean, unit},
    FunctionCallSyntax, OrderBy,
};
use uqa_core::{
    json::JsonToken,
    memory::{BudgetedVec, Produced},
};

impl<'a> Input<'a, '_> {
    pub(super) fn function_expression(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
    ) -> Result<Produced<Expr>, InputError> {
        let (mut name, mut binding, mut args, mut orders, mut filter) =
            (None, None, None, None, None);
        let (mut distinct, mut order_syntax) = (false, FunctionCallSyntax::Legacy);
        self.fields(
            token,
            &[
                "name",
                "binding",
                "args",
                "distinct",
                "order_by",
                "order_syntax",
                "filter",
            ],
            0b001_1101,
            |input, index, token| {
                match index {
                    0 => name = Some(input.text(token)?),
                    1 => binding = Some(input.maybe(token, Self::function_binding)?),
                    2 => args = Some(input.items(token, children)?),
                    3 => distinct = boolean(token)?,
                    4 => {
                        let mut index = 0;
                        orders = Some(input.array(token, |input, token| {
                            let order = input.call_order(token, children, index)?;
                            index += 1;
                            Ok(order)
                        })?);
                    }
                    5 => order_syntax = unit(&input.unit_tag(token)?)?,
                    6 => {
                        filter = Some(input.maybe(token, |input, token| {
                            input.child_box(token, children, Slot::First)
                        })?);
                    }
                    _ => unreachable!("declared function expression field"),
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = Expr::Func {
            name: parts.take(name.expect("required function name")),
            binding: binding.and_then(|value| parts.take(value)),
            args: parts.take(args.expect("required arguments")),
            distinct,
            order_by: parts.take(orders.expect("required orders")),
            order_syntax,
            filter: filter.and_then(|value| parts.take(value)),
        };
        parts.finish(value)
    }

    fn call_order(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
        index: usize,
    ) -> Result<Produced<OrderBy>, InputError> {
        let (mut expr, mut descending, mut nulls) = (None, false, None);
        self.fields(
            token,
            &["expr", "descending", "nulls"],
            3,
            |input, part, token| {
                match part {
                    0 => expr = Some(input.child(token, children, Slot::Order(index))?),
                    1 => descending = boolean(token)?,
                    _ if token == JsonToken::Null => (),
                    _ => nulls = Some(unit(&input.unit_tag(token)?)?),
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = OrderBy {
            expr: parts.take(expr.expect("required ordered expression")),
            descending,
            nulls,
        };
        parts.finish(value)
    }
}
