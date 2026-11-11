//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored schema expressions retain selected syntax without evaluating SQL. Unfinished expression parents live in admitted explicit frames.

use super::Expr;
use crate::{
    ast::stored_input::{invalid, Input, InputError, Parts, Values},
    SQLError,
};
use uqa_core::{
    json::JsonToken,
    memory::{BudgetedVec, MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};

mod composites;
mod fields;
mod functions;
mod nodes;

#[derive(Clone, Copy)]
enum Slot {
    First,
    Second,
    Third,
    Item(usize),
    Order(usize),
    When(usize, bool),
}

#[derive(Clone, Copy)]
struct Child<'a> {
    text: &'a str,
    depth: usize,
    slot: Slot,
}

struct Frame<'a> {
    expression: Produced<Expr>,
    children: BudgetedVec<Child<'a>>,
    cursor: usize,
}

impl Expr {
    /// Decode the stored scalar forms admitted in defaults, generated expressions, CHECKs and index expressions. Query/window nodes are rejected; selected names, bindings and native constants are retained without evaluation or semantic rebinding.
    pub fn from_stored_schema_json(
        text: &str,
        memory: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> Result<Produced<Self>, SQLError> {
        let values = Values {
            memory,
            original,
            invoking,
        };
        Ok(read(text, 128, &values)?)
    }
}

fn read(text: &str, depth: usize, values: &Values<'_>) -> Result<Produced<Expr>, InputError> {
    let control = values.control();
    {
        // Expr's mixed tagged/untagged serialization first reads generic content, including unknown fields, before selecting native scalar syntax.
        let mut input = Input::content_at_depth(text, control, depth);
        let token = input.next()?;
        input.content(token)?;
        input.end()?;
    }
    let mut parents = BudgetedVec::new(values.memory);
    let mut frame = Frame::read(text, depth, values)?;
    loop {
        control.check()?;
        if let Some(child) = frame.children.get(frame.cursor).copied() {
            parents.push(frame)?;
            frame = Frame::read(child.text, child.depth, values)?;
        } else if let Some(parent) = parents.pop() {
            frame = parent.attach(frame.expression, &control)?;
        } else {
            return Ok(frame.expression);
        }
    }
}

impl<'a> Frame<'a> {
    fn read(text: &'a str, depth: usize, values: &Values<'_>) -> Result<Self, InputError> {
        let mut input = Input::content_at_depth(text, values.control(), depth);
        let token = input.next()?;
        let mut children = BudgetedVec::new(values.memory);
        let expression = input.schema_node(token, &mut children, values)?;
        input.end()?;
        Ok(Self {
            expression,
            children,
            cursor: 0,
        })
    }

    fn attach(
        mut self,
        child: Produced<Expr>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, InputError> {
        let (mut expression, memory) = self.expression.into_parts();
        let (value, child_memory) = child.into_parts();
        *slot(&mut expression, self.children[self.cursor].slot) = value;
        self.expression = control.finish(expression, control.combine(memory, child_memory))?;
        self.cursor += 1;
        Ok(self)
    }
}

fn slot(expression: &mut Expr, slot: Slot) -> &mut Expr {
    match (expression, slot) {
        (
            Expr::UnaryMinus(value)
            | Expr::Not(value)
            | Expr::IsNull { expr: value, .. }
            | Expr::Cast { expr: value, .. }
            | Expr::Between { expr: value, .. }
            | Expr::InList { expr: value, .. },
            Slot::First,
        ) => value,
        (Expr::Binary { lhs, .. }, Slot::First) => lhs,
        (Expr::Binary { rhs, .. }, Slot::Second) => rhs,
        (Expr::Between { low, .. }, Slot::Second) => low,
        (Expr::Between { high, .. }, Slot::Third) => high,
        (
            Expr::Array(items)
            | Expr::Row(items)
            | Expr::And(items)
            | Expr::Or(items)
            | Expr::CompositeRow { items, .. }
            | Expr::InList { list: items, .. }
            | Expr::Func { args: items, .. },
            Slot::Item(index),
        ) => &mut items[index],
        (Expr::Func { order_by, .. }, Slot::Order(index)) => &mut order_by[index].expr,
        (
            Expr::Func {
                filter: Some(value),
                ..
            },
            Slot::First,
        )
        | (
            Expr::Case {
                base: Some(value), ..
            },
            Slot::First,
        )
        | (
            Expr::Case {
                else_branch: Some(value),
                ..
            },
            Slot::Second,
        ) => value,
        (Expr::Case { when, .. }, Slot::When(index, false)) => &mut when[index].0,
        (Expr::Case { when, .. }, Slot::When(index, true)) => &mut when[index].1,
        _ => unreachable!("child slot selected by its owning schema node"),
    }
}

impl<'a> Input<'a, '_> {
    pub(crate) fn schema_expression(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<Expr>, InputError> {
        let (text, depth) = self.encoded(token)?;
        read(text, depth, values)
    }

    fn child(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
        slot: Slot,
    ) -> Result<Produced<Expr>, InputError> {
        let (text, depth) = self.encoded(token)?;
        children.push(Child { text, depth, slot })?;
        Ok(self
            .control
            .finish(Expr::Star, self.control.empty_reservation())?)
    }

    fn child_box(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
        slot: Slot,
    ) -> Result<Produced<Box<Expr>>, InputError> {
        self.boxed(token, |input, token| input.child(token, children, slot))
    }

    fn items(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
    ) -> Result<Produced<Vec<Expr>>, InputError> {
        let mut index = 0;
        self.array(token, |input, token| {
            let child = input.child(token, children, Slot::Item(index))?;
            index += 1;
            Ok(child)
        })
    }
}

#[cfg(test)]
mod tests;
