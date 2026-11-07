//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native query images visit original fields incrementally without copying a plan.

use super::{invalid, DefinitionImageReader, Frame as ImageFrame, SQLError};
use crate::{plan::QueryPlan, ColumnIdentity};
use uqa_core::{
    memory::{MemoryBudget, MemoryReservation, OwnedSet, OwnedSetIter},
    CancellationToken,
};

pub(in super::super) const MAGIC: [u8; 8] = *b"UQAQRY01";
pub(in super::super) const EXPRESSION_MAGIC: [u8; 8] = *b"UQAXPL01";
pub(in super::super) const UNIFIED_MAGIC: [u8; 8] = *b"UQAPLN01";

pub(super) trait Node: Sync {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError>;
}

pub(super) enum Part<'a> {
    Node(&'a dyn Node),
    Text(&'a str),
    OptionalText(Option<&'a str>),
    Bytes(&'a [u8]),
    Tag(u8),
    Count(usize),
    U16(u16),
    U32(u32),
    U64(u64),
    Scalar(&'a crate::ScalarExpr),
    Expression(&'a crate::ast::Expr),
    Type(&'a crate::ColumnType),
    Binding(&'a crate::ast::FunctionBinding),
    ViewMap(views::Map<'a>),
    ViewAcl(&'a crate::catalog::security::table_binding::BoundTableAclEntry),
    Value(&'a uqa_core::Value),
    Columns(&'a OwnedSet<ColumnIdentity>),
    Positions(&'a OwnedSet<usize>),
    StatementMap(&'a uqa_core::memory::OwnedMap<String, crate::ast::MergeTargetColumnBinding>),
    Oids(&'a OwnedSet<u32>),
}

pub(super) enum Frame<'a> {
    Node(&'a dyn Node, usize),
    ViewMap(Box<views::Cursor<'a>>, MemoryReservation),
    Columns(Box<OwnedSetIter<'a, ColumnIdentity>>, MemoryReservation),
    Positions(Box<OwnedSetIter<'a, usize>>, MemoryReservation),
    StatementMap(Box<statements::ColumnCursor<'a>>, MemoryReservation),
    Oids(Box<OwnedSetIter<'a, u32>>, MemoryReservation),
}

fn fields<'a>(position: usize, values: &[&'a dyn Node]) -> Option<Part<'a>> {
    values.get(position).map(|value| Part::Node(*value))
}

fn variant<'a>(position: usize, tag: u8, values: &[&'a dyn Node]) -> Option<Part<'a>> {
    if position == 0 {
        Some(Part::Tag(tag))
    } else {
        fields(position - 1, values)
    }
}

macro_rules! image_struct {
    ($ty:ty { $($field:ident),* $(,)? } $(skip { $($skip:ident),* $(,)? })?) => {
        impl Node for $ty {
            fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
                let Self { $($field,)* $($($skip: _,)*)? } = self;
                Ok(fields(position, &[$($field),*]))
            }
        }
    };
}

macro_rules! image_enum {
    ($ty:ty { $($tag:literal => $name:ident $(($value:ident))? $({ $($field:ident),* $(,)? })?),* $(,)? } $(reject { $($rejected:pat => $message:literal),* $(,)? })?) => {
        impl Node for $ty {
            fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
                Ok(match self {
                    $(Self::$name $(($value))? $({$($field),*})? =>
                        variant(position, $tag, &[$($value,)? $($($field,)*)?]),)*
                    $($($rejected => return Err(invalid($message)),)*)?
                })
            }
        }
    };
}

mod metadata;
mod mutations;
mod primitives;
mod queries;
mod sources;
mod statements;
#[cfg(test)]
mod tests;
mod unified;
mod views;

impl QueryPlan {
    /// Stream the stored-syntax projection of this query, including all four kinds of data-modifying CTE. Analysis caches and execution-scoped sources do not enter a stored image.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        DefinitionImageReader::start(
            ImageFrame::Plan(Frame::Node(self, 0)),
            MAGIC,
            1,
            memory,
            cancellation,
        )
    }
}

impl crate::plan::ExpressionPlan {
    /// Stream executable scalar syntax together with its original ordered query arena. No SQL is parsed or evaluated and no catalog identity is rebound.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        DefinitionImageReader::start(
            ImageFrame::Plan(Frame::Node(self, 0)),
            EXPRESSION_MAGIC,
            1,
            memory,
            cancellation,
        )
    }
}

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn plan_part(&mut self, frame: Frame<'a>) -> Result<(), SQLError> {
        match frame {
            Frame::StatementMap(cursor, memory) => self.statement_columns(cursor, memory)?,
            Frame::Oids(mut values, memory) => {
                if let Some(value) = values.next() {
                    self.fields.scalar(value.to_le_bytes());
                    self.push(ImageFrame::Plan(Frame::Oids(values, memory)))?;
                }
            }
            Frame::ViewMap(cursor, memory) => self.view_map(cursor, memory)?,
            Frame::Node(node, position) => {
                let Some(part) = node.part(position)? else {
                    return Ok(());
                };
                self.push(ImageFrame::Plan(Frame::Node(node, position + 1)))?;
                self.plan_field(part)?;
            }
            Frame::Columns(mut values, memory) => {
                if let Some(value) = values.next() {
                    self.push(ImageFrame::Plan(Frame::Columns(values, memory)))?;
                    self.push(ImageFrame::Plan(Frame::Node(value, 0)))?;
                }
            }
            Frame::Positions(mut values, memory) => {
                if let Some(value) = values.next() {
                    self.fields.count(*value);
                    self.push(ImageFrame::Plan(Frame::Positions(values, memory)))?;
                }
            }
        }
        Ok(())
    }

    fn plan_field(&mut self, part: Part<'a>) -> Result<(), SQLError> {
        match part {
            Part::StatementMap(values) => self.start_statement_columns(values)?,
            Part::Oids(values) => {
                let memory = self.memory.reserve(size_of::<OwnedSetIter<'_, u32>>())?;
                self.push(ImageFrame::Plan(Frame::Oids(
                    Box::new(values.iter()),
                    memory,
                )))?;
            }
            Part::ViewMap(map) => self.start_view_map(map)?,
            Part::ViewAcl(acl) => self.fields.acl(acl),
            Part::Value(value) => self.push(ImageFrame::Value(value))?,
            Part::Node(node) => self.push(ImageFrame::Plan(Frame::Node(node, 0)))?,
            Part::Text(value) => self.fields.text(value),
            Part::OptionalText(value) => self.fields.optional_text(value),
            Part::Bytes(value) => self.fields.bytes(value),
            Part::Tag(value) => self.fields.scalar([value]),
            Part::Count(value) => self.fields.count(value),
            Part::U16(value) => self.fields.scalar(value.to_le_bytes()),
            Part::U32(value) => self.fields.scalar(value.to_le_bytes()),
            Part::U64(value) => self.fields.scalar(value.to_le_bytes()),
            Part::Scalar(value) => {
                self.push(ImageFrame::Scalar(super::scalars::Part::Node(value)))?;
            }
            Part::Expression(value) => self.push(ImageFrame::Node(value))?,
            Part::Type(value) => self.push(ImageFrame::Type(value))?,
            Part::Binding(value) => self.push(ImageFrame::Binding(value))?,
            Part::Columns(values) => {
                let memory = self
                    .memory
                    .reserve(size_of::<OwnedSetIter<'_, ColumnIdentity>>())?;
                self.push(ImageFrame::Plan(Frame::Columns(
                    Box::new(values.iter()),
                    memory,
                )))?;
            }
            Part::Positions(values) => {
                let memory = self.memory.reserve(size_of::<OwnedSetIter<'_, usize>>())?;
                self.push(ImageFrame::Plan(Frame::Positions(
                    Box::new(values.iter()),
                    memory,
                )))?;
            }
        }
        Ok(())
    }
}
