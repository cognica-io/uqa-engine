//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constructor scopes admit shared retirement depth before their structural children and retain complete prefixes through later fields.

use super::{Copying, Result};
use crate::plan::{
    copy::CopyBuffer,
    retirement::construction::{Context, Prefix, Root},
};
use std::ops::{Deref, DerefMut};
use uqa_core::memory::{BudgetedVec, MemoryReservation};

#[cfg(test)]
mod tests;

pub(super) struct Scope<'a, 'c> {
    copying: &'a mut Copying<'c>,
    previous: Option<Context>,
}
impl<'c> Deref for Scope<'_, 'c> {
    type Target = Copying<'c>;
    fn deref(&self) -> &Self::Target {
        self.copying
    }
}
impl DerefMut for Scope<'_, '_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.copying
    }
}
impl Drop for Scope<'_, '_> {
    fn drop(&mut self) {
        self.copying.plan_retirement = self.previous.take();
    }
}

impl<'c> Copying<'c> {
    pub(super) fn enter_plan_node(&mut self) -> Result<Scope<'_, 'c>> {
        let parent = match &self.plan_retirement {
            Some(parent) => parent.clone(),
            None => Context::new(&self.control)?,
        };
        let child = parent.child(&self.control)?;
        let previous = self.plan_retirement.replace(child);
        Ok(Scope {
            copying: self,
            previous,
        })
    }

    pub(super) fn retain_plan<T: Root>(&self, value: T) -> Prefix<T> {
        self.plan_retirement
            .as_ref()
            .expect("active plan constructor")
            .retain(value)
    }

    pub(super) fn lower_boxed<T: Root>(
        &mut self,
        copy: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<Box<T>> {
        let context = self
            .plan_retirement
            .as_ref()
            .expect("active plan box")
            .clone();
        self.boxed_with(copy, |value| context.retain(value), Prefix::take)
    }

    pub(super) fn map_plans<I: ExactSizeIterator, T: Root>(
        &mut self,
        items: I,
        copy: impl FnMut(&mut Self, I::Item) -> Result<T>,
    ) -> Result<Vec<T>> {
        let context = self
            .plan_retirement
            .as_ref()
            .expect("active plan vector")
            .clone();
        self.map_with_buffer(items, copy, |budget| {
            context.retain(BudgetedVec::new(budget))
        })
    }
}

impl<T: Root> CopyBuffer<T> for Prefix<BudgetedVec<T>> {
    fn values_mut(&mut self) -> &mut BudgetedVec<T> {
        self
    }
    fn into_parts(self) -> (Vec<T>, MemoryReservation) {
        self.take().into_parts()
    }
}
