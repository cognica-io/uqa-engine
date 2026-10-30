//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable syntax constructors keep their payload leases until the complete tree is consumed.

use super::{copy_syntax_with_control, syntax::SyntaxCopy};
use crate::SQLError;
use std::cell::RefCell;
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation, Produced, ProductionControl};

pub(crate) struct SyntaxProduction<'a> {
    control: ProductionControl<'a>,
    memory: RefCell<Option<MemoryReservation>>,
}

impl<'a> SyntaxProduction<'a> {
    pub(crate) fn new(control: ProductionControl<'a>) -> Self {
        Self {
            control,
            memory: RefCell::new(control.empty_reservation()),
        }
    }

    pub(crate) fn control(&self) -> ProductionControl<'a> {
        self.control
    }

    pub(crate) fn check(&self) -> Result<(), SQLError> {
        Ok(self.control.check()?)
    }

    pub(crate) fn take<T>(&self, value: Produced<T>) -> T {
        let (value, memory) = value.into_parts();
        self.absorb(memory);
        value
    }

    fn absorb(&self, memory: Option<MemoryReservation>) {
        let mut owned = self.memory.borrow_mut();
        *owned = self.control.combine(owned.take(), memory);
    }

    pub(crate) fn copy<T: SyntaxCopy + Clone>(&self, value: &T) -> Result<T, SQLError> {
        Ok(self.take(copy_syntax_with_control(value, &self.control)?))
    }

    pub(crate) fn text(&self, value: &str) -> Result<String, SQLError> {
        Ok(self.take(self.control.copy_text(value)?))
    }

    pub(crate) fn boxed<T>(&self, value: T) -> Result<Box<T>, SQLError> {
        let memory = self.control.reserve(size_of::<T>())?;
        let value = Box::new(value);
        self.absorb(memory);
        self.check()?;
        Ok(value)
    }

    pub(crate) fn set_copy<T: Copy + Ord>(
        &self,
        values: impl IntoIterator<Item = T>,
    ) -> Result<uqa_core::memory::OwnedSet<T>, SQLError> {
        let mut output = uqa_core::memory::OwnedSet::new();
        for value in values {
            self.check()?;
            if !output.contains(&value) {
                let memory = self
                    .control
                    .reserve(uqa_core::memory::OwnedSet::<T>::entry_bytes())?;
                output.insert(value);
                self.absorb(memory);
            }
        }
        Ok(output)
    }

    pub(crate) fn vector<T>(&self, capacity: usize) -> Result<SyntaxVector<'_, 'a, T>, SQLError> {
        let values = self.control.budget().map_or_else(
            || SyntaxBuffer::Ordinary(Vec::new()),
            |budget| SyntaxBuffer::Controlled(BudgetedVec::new(budget)),
        );
        let mut values = SyntaxVector {
            values,
            output: self,
        };
        values.reserve(capacity)?;
        Ok(values)
    }

    /// Move expansion elements into the final destination while retiring their old vector buffer independently. The element payloads remain in this immutable tree's accumulating owner.
    pub(crate) fn elements<T>(&self, values: Produced<Vec<T>>) -> SyntaxElements<T> {
        let (values, mut memory) = values.into_parts();
        let buffer = memory
            .as_mut()
            .map(|memory| memory.split(values.capacity() * size_of::<T>()));
        self.absorb(memory);
        SyntaxElements {
            values: values.into_iter(),
            _memory: buffer,
        }
    }

    pub(crate) fn finish<T>(self, value: T) -> Result<Produced<T>, SQLError> {
        Ok(self.control.finish(value, self.memory.into_inner())?)
    }
}

pub(crate) struct SyntaxElements<T> {
    values: std::vec::IntoIter<T>,
    _memory: Option<MemoryReservation>,
}
impl<T> Iterator for SyntaxElements<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.values.next()
    }
}

/// Element payloads have already been admitted by this syntax producer; the buffer retains its own capacity until it joins the completed tree.
pub(crate) struct SyntaxVector<'output, 'control, T> {
    values: SyntaxBuffer<T>,
    output: &'output SyntaxProduction<'control>,
}

enum SyntaxBuffer<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

impl<T> SyntaxVector<'_, '_, T> {
    fn reserve(&mut self, additional: usize) -> Result<(), SQLError> {
        self.output.check()?;
        match &mut self.values {
            SyntaxBuffer::Ordinary(values) => {
                values.try_reserve(additional).map_err(MemoryError::from)?;
            }
            SyntaxBuffer::Controlled(values) => values.reserve(additional)?,
        }
        Ok(())
    }

    pub(crate) fn push(&mut self, value: T) -> Result<(), SQLError> {
        self.reserve(1)?;
        match &mut self.values {
            SyntaxBuffer::Ordinary(values) => values.push(value),
            SyntaxBuffer::Controlled(values) => values.push(value)?,
        }
        Ok(())
    }
    pub(crate) fn finish(self) -> Result<Vec<T>, SQLError> {
        self.output.check()?;
        Ok(match self.values {
            SyntaxBuffer::Ordinary(values) => values,
            SyntaxBuffer::Controlled(values) => {
                let (values, memory) = values.into_parts();
                self.output.absorb(Some(memory));
                values
            }
        })
    }
}

pub(crate) trait CollectSyntax<T>: Iterator<Item = Result<T, SQLError>> + Sized {
    fn collect_syntax(self, output: &SyntaxProduction<'_>) -> Result<Vec<T>, SQLError> {
        let mut result = output.vector(self.size_hint().0)?;
        for value in self {
            output.check()?;
            result.push(value?)?;
        }
        result.finish()
    }
}
impl<T, I: Iterator<Item = Result<T, SQLError>>> CollectSyntax<T> for I {}
