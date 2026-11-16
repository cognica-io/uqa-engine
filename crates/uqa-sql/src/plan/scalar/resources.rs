//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Destination admission for the shared AST-to-scalar lowerer.

pub(super) mod input;

use super::source::Source;
use crate::schema::retention::CatalogRetentionError;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation, ProductionControl},
    CancellationToken, QueryCancelled, Value,
};

pub(super) type Result<T> = std::result::Result<T, CatalogRetentionError>;

pub(super) struct Control<'a> {
    pub(super) memory: MemoryReservation,
    pub(super) production: ProductionControl<'a>,
}

impl<'a> Control<'a> {
    pub(super) fn from_production(production: ProductionControl<'a>) -> Self {
        Self {
            memory: production
                .empty_reservation()
                .expect("controlled scalar copy"),
            production,
        }
    }

    pub(super) fn new(
        budget: &'a MemoryBudget,
        original: &'a CancellationToken,
        invoking: &'a CancellationToken,
    ) -> Self {
        Self {
            memory: budget.empty_reservation(),
            production: ProductionControl::new(budget, original, invoking),
        }
    }

    pub(super) fn check(&self) -> std::result::Result<(), QueryCancelled> {
        self.production.check_cancellation()
    }
}

pub(super) struct Lowering<'a> {
    pub(super) control: Option<Control<'a>>,
    pub(super) owned_children: Option<BudgetedVec<Option<super::owned::ScalarMemory>>>,
}

impl Lowering<'_> {
    pub(super) fn check(&self) -> Result<()> {
        if let Some(control) = &self.control {
            control.check()?;
        }
        Ok(())
    }

    pub(super) fn map<I, T>(
        &mut self,
        items: I,
        mut convert: impl FnMut(&mut Self, I::Item) -> Result<T>,
    ) -> Result<Vec<T>>
    where
        I: ExactSizeIterator,
    {
        self.check()?;
        let Some(control) = &self.control else {
            return items.map(|item| convert(self, item)).collect();
        };
        let mut output = BudgetedVec::new(control.memory.budget());
        output.reserve(items.len())?;
        for item in items {
            self.check()?;
            output.push(convert(self, item)?)?;
        }
        let (output, memory) = output.into_parts();
        self.control
            .as_mut()
            .expect("controlled lowering")
            .memory
            .absorb(memory);
        Ok(output)
    }

    pub(super) fn boxed<T>(
        &mut self,
        produce: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<Box<T>> {
        self.check()?;
        if let Some(control) = &mut self.control {
            control.memory.grow(size_of::<T>())?;
        }
        let value = produce(self)?;
        self.check()?;
        Ok(Box::new(value))
    }

    pub(super) fn text(&mut self, source: Source<'_, String>) -> Result<String> {
        self.check()?;
        match source {
            Source::Owned(text) => Ok(text),
            Source::Borrowed(text) => self.copy_text(text),
        }
    }

    pub(super) fn reference_name(
        &mut self,
        source: Source<'_, crate::ast::ReferenceName>,
    ) -> Result<crate::ast::ReferenceName> {
        self.check()?;
        match source {
            Source::Owned(name) => Ok(name),
            Source::Borrowed(name) => self.copy_reference_name(name),
        }
    }

    pub(super) fn copy_reference_name(
        &mut self,
        name: &crate::ast::ReferenceName,
    ) -> Result<crate::ast::ReferenceName> {
        Ok(crate::ast::ReferenceName::from(self.copy_text(name)?).with_position(name.position()))
    }

    pub(super) fn copy_text(&mut self, text: &str) -> Result<String> {
        let Some(control) = self.control.as_mut() else {
            return Ok(text.to_owned());
        };
        let copied = control.production.copy_text(text)?;
        let (output, memory) = copied.into_parts();
        control
            .memory
            .absorb(memory.expect("controlled text production"));
        Ok(output)
    }

    pub(super) fn composite_binding(
        &mut self,
        binding: Source<'_, crate::ast::CompositeRowBinding>,
    ) -> Result<crate::ast::CompositeRowBinding> {
        match binding {
            Source::Owned(binding) => Ok(binding),
            Source::Borrowed(binding) => {
                crate::catalog::work::inline(self.composite_binding_from(binding, &mut ()))
            }
        }
    }

    pub(super) fn composite_source(
        &mut self,
        source: Option<Source<'_, Box<crate::expr::composites::CompositeConstantSource>>>,
    ) -> Result<Option<Box<crate::expr::composites::CompositeConstantSource>>> {
        match source {
            Some(Source::Owned(source)) => Ok(Some(source)),
            Some(Source::Borrowed(source)) => crate::catalog::work::inline(
                self.composite_source_from(Some(source.as_ref()), &mut ()),
            ),
            None => Ok(None),
        }
    }

    pub(super) fn value(&mut self, source: Source<'_, Value>) -> Result<Value> {
        self.check()?;
        match source {
            Source::Owned(value) => Ok(value),
            Source::Borrowed(value) => {
                let Some(control) = self.control.as_mut() else {
                    return Ok(value.clone());
                };
                let copied = control.production.copy_value(value)?;
                let (value, memory) = copied.into_parts();
                control
                    .memory
                    .absorb(memory.expect("controlled value production"));
                Ok(value)
            }
        }
    }

    pub(super) fn finish<T>(self, value: T) -> Result<Budgeted<T>> {
        self.check()?;
        let control = self.control.expect("controlled lowering returns its lease");
        Ok(Budgeted::new(value, control.memory))
    }
}
