//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Child, Expr, Input, InputError, Parts, Slot};
use crate::ast::stored_input::{boolean, unit};
use uqa_core::{
    json::JsonToken,
    memory::{BudgetedVec, Produced},
};

impl<'a> Input<'a, '_> {
    pub(super) fn binary_expression(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
    ) -> Result<Produced<Expr>, InputError> {
        let (mut op, mut lhs, mut rhs) = (None, None, None);
        self.fields(token, &["op", "lhs", "rhs"], 7, |input, index, token| {
            match index {
                0 => op = Some(unit(&input.unit_tag(token)?)?),
                1 => lhs = Some(input.child_box(token, children, Slot::First)?),
                _ => rhs = Some(input.child_box(token, children, Slot::Second)?),
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let value = Expr::Binary {
            op: op.expect("required operator"),
            lhs: parts.take(lhs.expect("required left operand")),
            rhs: parts.take(rhs.expect("required right operand")),
        };
        parts.finish(value)
    }

    pub(super) fn structured_expression(
        &mut self,
        tag: &str,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
    ) -> Result<Produced<Expr>, InputError> {
        let (names, required): (&[&str], _) = match tag {
            "IsNull" => (&["expr", "negated"], 3),
            "InList" => (&["expr", "list", "negated"], 7),
            "Between" => (&["expr", "low", "high"], 7),
            _ => (&["implicit", "expr", "ty"], 6),
        };
        let (mut expr, mut low, mut high, mut list, mut ty) = (None, None, None, None, None);
        let mut flag = false;
        self.fields(token, names, required, |input, index, token| {
            match names[index] {
                "expr" => expr = Some(input.child_box(token, children, Slot::First)?),
                "low" => low = Some(input.child_box(token, children, Slot::Second)?),
                "high" => high = Some(input.child_box(token, children, Slot::Third)?),
                "list" => list = Some(input.items(token, children)?),
                "ty" => ty = Some(input.text(token)?),
                _ => flag = boolean(token)?,
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let expr = parts.take(expr.expect("required expression"));
        let value = match tag {
            "IsNull" => Expr::IsNull {
                expr,
                negated: flag,
            },
            "InList" => Expr::InList {
                expr,
                list: parts.take(list.expect("required list")),
                negated: flag,
            },
            "Between" => Expr::Between {
                expr,
                low: parts.take(low.expect("required lower bound")),
                high: parts.take(high.expect("required upper bound")),
            },
            _ => Expr::Cast {
                implicit: flag,
                expr,
                ty: parts.take(ty.expect("required type")),
            },
        };
        parts.finish(value)
    }

    pub(super) fn case_expression(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
    ) -> Result<Produced<Expr>, InputError> {
        let (mut base, mut when, mut otherwise) = (None, None, None);
        self.fields(
            token,
            &["base", "when", "else_branch"],
            2,
            |input, index, token| {
                match index {
                    0 => {
                        base = Some(input.maybe(token, |input, token| {
                            input.child_box(token, children, Slot::First)
                        })?);
                    }
                    1 => {
                        let mut index = 0;
                        when = Some(input.array(token, |input, token| {
                            let pair = input.case_pair(token, children, index)?;
                            index += 1;
                            Ok(pair)
                        })?);
                    }
                    _ => {
                        otherwise = Some(input.maybe(token, |input, token| {
                            input.child_box(token, children, Slot::Second)
                        })?);
                    }
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = Expr::Case {
            base: base.and_then(|value| parts.take(value)),
            when: parts.take(when.expect("required CASE branches")),
            else_branch: otherwise.and_then(|value| parts.take(value)),
        };
        parts.finish(value)
    }

    fn case_pair(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
        index: usize,
    ) -> Result<Produced<(Expr, Expr)>, InputError> {
        if token != JsonToken::StartArray {
            return Err(invalid());
        }
        let mut pair = (None, None);
        self.fields(token, &["condition", "result"], 3, |input, part, token| {
            let value = input.child(token, children, Slot::When(index, part == 1))?;
            if part == 0 {
                pair.0 = Some(value);
            } else {
                pair.1 = Some(value);
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let value = (
            parts.take(pair.0.expect("required condition")),
            parts.take(pair.1.expect("required result")),
        );
        parts.finish(value)
    }
}
