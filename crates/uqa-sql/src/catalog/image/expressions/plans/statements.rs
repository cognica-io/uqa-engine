//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored bodies retain their original statement syntax independently of bound execution plans.

use super::{
    fields, invalid, variant, DefinitionImageReader, Frame, ImageFrame, Node, Part, SQLError,
};
use crate::ast::{Expr, Statement};
use uqa_core::{
    memory::{BudgetedMapIter, MemoryBudget, MemoryReservation, OwnedMap, OwnedSet},
    CancellationToken,
};

mod metadata;
mod mutations;
mod queries;
mod sources;
#[cfg(test)]
mod tests;

pub(in super::super) type ColumnCursor<'a> =
    BudgetedMapIter<'a, String, crate::ast::MergeTargetColumnBinding>;

impl Statement {
    /// Borrow original stored query, mutation or NOTIFY syntax without lowering, executing or resolving it. Other utility statements are not durable routine/rule body members.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        let mut reader = DefinitionImageReader::start(
            ImageFrame::Plan(Frame::Node(self, 0)),
            *b"UQASTM01",
            1,
            memory,
            cancellation,
        )?;
        let _ = self.part(0)?;
        reader.statement_expressions = true;
        Ok(reader)
    }
}

impl Node for Statement {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match self {
            Self::Select(body) => variant(position, 0, &[body]),
            Self::Values { rows } => variant(position, 1, &[rows]),
            Self::Insert(body) => variant(position, 2, &[body]),
            Self::Update(body) => variant(position, 3, &[body]),
            Self::Delete(body) => variant(position, 4, &[body]),
            Self::Merge(body) => variant(position, 5, &[body]),
            Self::Notify { channel, payload } => variant(position, 6, &[channel, payload]),
            _ => return Err(invalid("non-stored body statement")),
        })
    }
}

impl Node for Expr {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match self {
            Self::ScalarSubquery(body) => variant(position, 24, &[body]),
            Self::Exists { body, negated } => variant(position, 25, &[negated, body]),
            Self::InSubquery {
                expr,
                body,
                negated,
            } => variant(position, 26, &[negated, expr, body]),
            Self::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => variant(position, 27, &[name, args, spec, filter, modifiers]),
            _ => (position == 0).then_some(Part::Expression(self)),
        })
    }
}

impl Node for i16 {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok((position == 0).then_some(Part::U16(*self as u16)))
    }
}

impl Node for OwnedSet<u32> {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match position {
            0 => Some(Part::Count(self.len())),
            1 => Some(Part::Oids(self)),
            _ => None,
        })
    }
}

impl Node for OwnedMap<String, crate::ast::MergeTargetColumnBinding> {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match position {
            0 => Some(Part::Count(self.len())),
            1 => Some(Part::StatementMap(self)),
            _ => None,
        })
    }
}

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn start_statement_columns(
        &mut self,
        values: &'a OwnedMap<String, crate::ast::MergeTargetColumnBinding>,
    ) -> Result<(), SQLError> {
        let memory = self.memory.reserve(size_of::<ColumnCursor<'_>>())?;
        self.push(ImageFrame::Plan(Frame::StatementMap(
            Box::new(values.iter()),
            memory,
        )))
    }

    pub(super) fn statement_columns(
        &mut self,
        mut cursor: Box<ColumnCursor<'a>>,
        memory: MemoryReservation,
    ) -> Result<(), SQLError> {
        if let Some((name, value)) = cursor.next() {
            self.fields.text(name);
            self.push(ImageFrame::Plan(Frame::StatementMap(cursor, memory)))?;
            self.push(ImageFrame::Plan(Frame::Node(value, 0)))?;
        }
        Ok(())
    }
}
