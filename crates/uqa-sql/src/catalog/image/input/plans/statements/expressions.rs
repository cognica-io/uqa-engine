//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Body, Empty, Field, Fields, Input, PlanMemory, SQLError, Step};
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

macro_rules! borrowed_syntax_fields {
    ($position:expr; $($field:expr),*; $($unused:expr),*) => {{
        let fields: [&mut dyn Field; _] = [$($field as &mut dyn Field),*];
        fields.into_iter().nth($position)
    }};
}
macro_rules! native_syntax_fields {
    ($position:expr; $($field:expr),*; $input:expr, $memory:expr) => {
        read_syntax_fields!($position, $input, $memory; $($field),*)
    };
}
macro_rules! expression_fields {
    ($owner:expr, $index:expr, $select:ident $(, $args:expr)*) => {{
        let index = $index;
        match $owner {
            Expr::ScalarSubquery(body) => $select!(index; body; $($args),*),
            Expr::Exists { body, negated } => {
                $select!(index; negated, body; $($args),*)
            }
            Expr::InSubquery {
                expr,
                body,
                negated,
            } => {
                $select!(index; negated, expr, body; $($args),*)
            }
            Expr::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => {
                $select!(index; name, args, spec, filter, modifiers; $($args),*)
            }
            value => match super::super::super::expressions::expression_child(value, index) {
                Some(value) => $select!(0; value; $($args),*),
                None => $select!(0;; $($args),*),
            },
        }
    }};
}
macro_rules! read_expression {
    ($owner:expr, $position:expr, $input:expr, $memory:expr, $read:ident, $select:ident) => {{
        if $position == 0 {
            *$owner = $read!($input.statement_expression($memory))?;
            return Ok(Step::Advance);
        }
        if !matches!(
            $owner,
            Expr::ScalarSubquery(_)
                | Expr::Exists { .. }
                | Expr::InSubquery { .. }
                | Expr::WindowCall { .. }
        ) && !$read!($input.prepare_expression_child($owner, $position - 1))?
        {
            return Ok(Step::Done);
        }
        $select!($owner, $position, $input, $memory)
    }};
}
macro_rules! read_borrowed_expression {
    ($owner:expr, $position:expr, $input:expr, $memory:expr) => {
        $owner
            .field($position)
            .map_or(Ok(Step::Done), |field| field.read($input, $memory))
    };
}
macro_rules! read_native_expression {
    ($owner:expr, $position:expr, $input:expr, $memory:expr) => {
        expression_fields!($owner, $position - 1, native_syntax_fields, $input, $memory)
    };
}

impl Body for Expr {
    fn new(_: &mut PlanMemory) -> Result<Self, SQLError> {
        Ok(Self::Star)
    }
    fn first_field(&self) -> usize {
        1
    }
    fn field(&mut self, position: usize) -> Option<&mut dyn Field> {
        expression_fields!(self, position.checked_sub(1)?, borrowed_syntax_fields)
    }
    fn read(
        &mut self,
        position: usize,
        _: &mut Option<usize>,
        input: &mut Input<'_>,
        memory: &mut PlanMemory,
    ) -> Result<Step, SQLError> {
        read_expression!(
            self,
            position,
            input,
            memory,
            read_field,
            read_borrowed_expression
        )
    }
    async fn read_fields(
        &mut self,
        position: usize,
        _: &mut Option<usize>,
        input: &mut Fields<'_>,
        memory: &mut PlanMemory,
    ) -> Result<Step, SQLError> {
        read_expression!(
            self,
            position,
            input,
            memory,
            await_field,
            read_native_expression
        )
    }
}

macro_rules! statement_expression_header {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            $($async)? fn statement_expression(&mut self, memory: &mut PlanMemory) -> Result<Expr, SQLError> {
                Ok(match $read!(self.binary.array::<1>())?[0] {
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
                    tag => $read!(self.statement_scalar_tag(tag, memory))?,
                })
            }
        }
    };
}
statement_expression_header!(Input, read_field);
statement_expression_header!(Fields, await_field, async);
impl Input<'_> {
    fn statement_scalar_tag(&mut self, tag: u8, memory: &mut PlanMemory) -> Result<Expr, SQLError> {
        self.expression_tag(tag, memory)
    }
}
impl Fields<'_> {
    async fn statement_scalar_tag(
        &mut self,
        tag: u8,
        memory: &mut PlanMemory,
    ) -> Result<Expr, SQLError> {
        let mut value = Expr::Star;
        await_field!(self.expression_tag(tag, memory, &mut value))?;
        Ok(value)
    }
}
