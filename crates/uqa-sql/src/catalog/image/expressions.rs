//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed stored-expression images retain original syntax and evaluated native metadata.

use super::{
    fields::{Field, Fields},
    invalid, CatalogImageReader,
};
use crate::{
    ast::{ColumnType, Expr, FunctionBinding, OrderBy},
    expr::composites::{
        CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor,
        DroppedCompositeAttribute,
    },
    SQLError,
};
use uqa_core::{
    memory::{BudgetedVec, MemoryBudget, MemoryReservation},
    value_image::{ValueImageError, ValueImageReader},
    CancellationToken, Value,
};

mod domains;
mod nodes;
mod parts;
#[cfg(test)]
mod tests;

pub(super) const MAGIC: [u8; 8] = *b"UQAEXP01";
const WINDOW: usize = 16384;

enum Frame<'a> {
    NextDomain,
    DomainDefinition(&'a crate::ast::CreateDomain),
    DomainChecks(&'a [crate::ast::DomainCheck]),
    DomainNotNull(&'a crate::ast::DomainNotNull),
    ObjectAcls(&'a [crate::ast::ObjectAclEntry]),
    Node(&'a Expr),
    Nodes(&'a [Expr]),
    Orders(&'a [OrderBy]),
    Cases(&'a [(Expr, Expr)]),
    Binding(&'a FunctionBinding),
    Value(&'a Value),
    Type(&'a ColumnType),
    Types(&'a [ColumnType]),
    AttributeNumbers(&'a [i16]),
    CompositeSource(&'a CompositeConstantSource),
    Descriptors(&'a [CompositeTypeDescriptor]),
    Attributes(&'a [CompositeAttribute]),
    Dropped(&'a [DroppedCompositeAttribute]),
}

enum Child<'a> {
    Binding {
        reader: Box<CatalogImageReader<'a>>,
        _memory: MemoryReservation,
    },
    Value {
        reader: Box<ValueImageReader<'a, 'a>>,
        _memory: MemoryReservation,
    },
}

#[derive(Clone, Copy)]
enum State {
    Active,
    Complete,
    Failed,
}

/// Stream one captured scalar definition without copying its syntax or resolving names. Preparation borrows the root without visiting children; subsequent reads admit their traversal stack and return at most 16 KiB. Query/window nodes are outside the existing stored-definition capture contract. Retained literals must expose their captured resident value.
pub struct DefinitionImageReader<'a> {
    first: Option<Frame<'a>>,
    domains: Option<
        crate::catalog::named_definitions::CapturedDefinitionIter<
            'a,
            String,
            crate::catalog::domain::StoredDomain,
        >,
    >,
    frames: BudgetedVec<Frame<'a>>,
    fields: Fields<'a>,
    field: Field<'a>,
    offset: usize,
    child: Option<Child<'a>>,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    state: State,
}

impl Expr {
    /// Reconstruct one stored scalar without resolving names or executing its values. Native allocations remain charged to the returned owner; malformed or interrupted input retires partial trees with constant scalar stack.
    pub fn read_image(
        reader: &mut dyn std::io::Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<uqa_core::memory::Budgeted<Self>, SQLError> {
        super::input::expression_image(reader, memory, cancellation)
    }

    /// Borrow captured scalar syntax and its native metadata; every read is bounded and traversal scratch remains admitted.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        DefinitionImageReader::new(self, memory, cancellation)
    }
}

impl<'a> DefinitionImageReader<'a> {
    fn new(
        value: &'a Expr,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<Self, SQLError> {
        cancellation.check()?;
        let mut fields = Fields::new();
        fields.scalar(MAGIC);
        fields.count(1);
        Ok(Self {
            first: Some(Frame::Node(value)),
            domains: None,
            frames: BudgetedVec::new(memory),
            fields,
            field: Field::Bytes(&[]),
            offset: 0,
            child: None,
            memory,
            cancellation,
            state: State::Active,
        })
    }

    pub fn read(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        match self.state {
            State::Complete => return Ok(0),
            State::Failed => return Err(invalid("failed expression image reader")),
            State::Active => {}
        }
        let mut advance = Advance {
            reader: self,
            succeeded: false,
        };
        let count = advance.reader.read_active(output)?;
        advance.succeeded = true;
        Ok(count)
    }

    fn read_active(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        self.cancellation.check()?;
        let limit = output.len().min(WINDOW);
        let mut written = 0;
        while written < limit {
            self.cancellation.check()?;
            let bytes = self.field.bytes();
            if self.offset < bytes.len() {
                let count = (limit - written).min(bytes.len() - self.offset).min(4096);
                output[written..written + count]
                    .copy_from_slice(&bytes[self.offset..self.offset + count]);
                self.offset += count;
                written += count;
            } else if let Some(field) = self.fields.next() {
                self.field = field;
                self.offset = 0;
            } else if let Some(child) = self.child.as_mut() {
                let count = match child {
                    Child::Binding { reader, .. } => reader.read(&mut output[written..limit])?,
                    Child::Value { reader, .. } => reader
                        .read(&mut output[written..limit])
                        .map_err(value_error)?,
                };
                written += count;
                if count == 0 {
                    self.child = None;
                }
            } else {
                let next = self.first.take().or_else(|| self.frames.pop());
                let Some(next) = next else {
                    self.frames = BudgetedVec::new(self.memory);
                    self.state = State::Complete;
                    break;
                };
                self.fields = Fields::new();
                self.advance(next)?;
            }
        }
        self.cancellation.check()?;
        Ok(written)
    }

    fn push(&mut self, frame: Frame<'a>) -> Result<(), SQLError> {
        self.frames.push(frame)?;
        Ok(())
    }

    fn optional_node(&mut self, value: Option<&'a Expr>) -> Result<(), SQLError> {
        if let Some(value) = value {
            self.push(Frame::Node(value))?;
        }
        Ok(())
    }

    fn fail(&mut self) {
        self.state = State::Failed;
        self.first = None;
        self.domains = None;
        self.child = None;
        self.frames = BudgetedVec::new(self.memory);
        self.fields = Fields::new();
        self.field = Field::Bytes(&[]);
        self.offset = 0;
    }
}

struct Advance<'reader, 'source> {
    reader: &'reader mut DefinitionImageReader<'source>,
    succeeded: bool,
}
impl Drop for Advance<'_, '_> {
    fn drop(&mut self) {
        if !self.succeeded {
            self.reader.fail();
        }
    }
}

pub(super) fn value_error(error: ValueImageError) -> SQLError {
    match error {
        ValueImageError::Invalid(message) => invalid(message),
        ValueImageError::Memory(error) => error.into(),
        ValueImageError::Cancelled(error) => error.into(),
        ValueImageError::Io(error) => SQLError::Host(std::sync::Arc::new(error)),
    }
}
