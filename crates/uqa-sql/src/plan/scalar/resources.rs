//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Destination admission for the shared AST-to-scalar lowerer.

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

    pub(super) fn copy_text(&mut self, text: &str) -> Result<String> {
        let control = self
            .control
            .as_mut()
            .expect("borrowed AST uses controlled lowering");
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
            Source::Borrowed(binding) => Ok(crate::ast::CompositeRowBinding {
                ty: self.copy_text(&binding.ty)?,
                attributes: self.map(binding.attributes.iter(), |_, number| Ok(*number))?,
                argument_types: binding
                    .argument_types
                    .as_ref()
                    .map(|types| {
                        self.map(types.iter(), |this, ty| {
                            Ok(this
                                .scalar_type_copy(Some(ty))?
                                .expect("constructor argument type"))
                        })
                    })
                    .transpose()?,
            }),
        }
    }

    pub(super) fn composite_source(
        &mut self,
        source: Option<Source<'_, Box<crate::expr::composites::CompositeConstantSource>>>,
    ) -> Result<Option<Box<crate::expr::composites::CompositeConstantSource>>> {
        use crate::expr::composites::{
            CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor,
        };
        source
            .map(|source| match source {
                Source::Owned(source) => Ok(source),
                Source::Borrowed(source) => self.boxed(|this| {
                    Ok(CompositeConstantSource {
                        value: this.value(Source::Borrowed(&source.value))?,
                        descriptors: this.map(source.descriptors.iter(), |this, descriptor| {
                            Ok(CompositeTypeDescriptor {
                                type_oid: descriptor.type_oid,
                                relation_oid: descriptor.relation_oid,
                                attributes: this.map(
                                    descriptor.attributes.iter(),
                                    |this, attribute| {
                                        Ok(CompositeAttribute {
                                            name: this.copy_text(&attribute.name)?,
                                            ty: this
                                                .scalar_type_copy(Some(&attribute.ty))?
                                                .expect("source attribute type"),
                                            number: attribute.number,
                                        })
                                    },
                                )?,
                            })
                        })?,
                    })
                }),
            })
            .transpose()
    }

    pub(super) fn value(&mut self, source: Source<'_, Value>) -> Result<Value> {
        self.check()?;
        match source {
            Source::Owned(value) => Ok(value),
            Source::Borrowed(value) => {
                let control = self
                    .control
                    .as_mut()
                    .expect("borrowed AST uses controlled lowering");
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
