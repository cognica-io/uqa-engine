//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Body, Empty, Field, Input, PlanMemory, SQLError, Step};
use crate::ast::{Expr, FrameBound, WindowCallModifiers};

syntax_enum!(FrameBound, FrameBound::CurrentRow;
    0 => UnboundedPreceding, 1 => UnboundedFollowing, 2 => CurrentRow,
    3 => Preceding(value), 4 => Following(value),
);

impl Empty for Expr {
    fn empty() -> Self {
        Self::Star
    }
    fn is_flat(&self) -> bool {
        matches!(
            self,
            Self::Star
                | Self::Default
                | Self::Column(_)
                | Self::QualifiedColumn { .. }
                | Self::QualifiedStar(_)
                | Self::InternalColumn(_)
                | Self::Literal(_)
                | Self::TypedLiteral { .. }
                | Self::RetainedLiteral(_)
                | Self::Param(_)
        )
    }
}

impl Body for Expr {
    fn new(_: &mut PlanMemory) -> Result<Self, SQLError> {
        Ok(Self::Star)
    }
    fn first_field(&self) -> usize {
        1
    }

    fn field(&mut self, position: usize) -> Option<&mut dyn Field> {
        let index = position.checked_sub(1)?;
        match self {
            Self::ScalarSubquery(body) => (index == 0).then_some(body as &mut dyn Field),
            Self::Exists { body, negated } => {
                let fields: [&mut dyn Field; 2] = [negated, body];
                fields.into_iter().nth(index)
            }
            Self::InSubquery {
                expr,
                body,
                negated,
            } => {
                let fields: [&mut dyn Field; 3] = [negated, expr, body];
                fields.into_iter().nth(index)
            }
            Self::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => {
                let fields: [&mut dyn Field; 5] = [name, args, spec, filter, modifiers];
                fields.into_iter().nth(index)
            }
            _ => super::super::super::expressions::expression_child(self, index)
                .map(|value| value as &mut dyn Field),
        }
    }

    fn read(
        &mut self,
        position: usize,
        _: &mut Option<usize>,
        input: &mut Input<'_>,
        memory: &mut PlanMemory,
    ) -> Result<Step, SQLError> {
        if position == 0 {
            *self = input.statement_expression(memory)?;
            return Ok(Step::Advance);
        }
        if !matches!(
            self,
            Self::ScalarSubquery(_)
                | Self::Exists { .. }
                | Self::InSubquery { .. }
                | Self::WindowCall { .. }
        ) && !input.prepare_expression_child(self, position - 1)?
        {
            return Ok(Step::Done);
        }
        self.field(position)
            .map_or(Ok(Step::Done), |field| field.read(input, memory))
    }
}

impl Input<'_> {
    fn statement_expression(&mut self, memory: &mut PlanMemory) -> Result<Expr, SQLError> {
        Ok(match self.binary.array::<1>()?[0] {
            24 => Expr::ScalarSubquery(Field::new(memory)?),
            25 => Expr::Exists {
                body: Field::new(memory)?,
                negated: false,
            },
            26 => Expr::InSubquery {
                expr: Field::new(memory)?,
                body: Field::new(memory)?,
                negated: false,
            },
            27 => Expr::WindowCall {
                name: String::new(),
                args: Vec::new(),
                spec: Field::new(memory)?,
                filter: None,
                modifiers: WindowCallModifiers::default(),
            },
            tag => self.expression_tag(tag, memory)?,
        })
    }
}
