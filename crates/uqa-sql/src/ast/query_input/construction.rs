//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native child results retain admitted storage independently of the call stack.

use super::{InputError, Produced};
use uqa_core::memory::{MemoryReservation, ProductionControl};

pub(in crate::ast) struct Native<T> {
    value: Box<Produced<T>>,
    memory: Option<MemoryReservation>,
}

impl<T> Native<T> {
    pub(in crate::ast) fn new(
        value: Produced<T>,
        control: ProductionControl<'_>,
    ) -> Result<Self, InputError> {
        let memory = control.reserve(size_of::<Produced<T>>())?;
        Ok(Self {
            value: Box::new(value),
            memory,
        })
    }

    pub(in crate::ast) fn into_parts(self) -> (T, Option<MemoryReservation>) {
        Produced::from(self).into_parts()
    }
}

impl<T> From<Native<T>> for Produced<T> {
    fn from(value: Native<T>) -> Self {
        let Native { value, memory } = value;
        // Deallocate the temporary result box before releasing its allowance.
        let result = {
            let owned = value;
            *owned
        };
        drop(memory);
        result
    }
}

pub(super) struct Parts<'c> {
    values: crate::ast::stored_input::Parts<'c>,
    control: ProductionControl<'c>,
}

impl<'c> Parts<'c> {
    pub(super) fn new(control: ProductionControl<'c>) -> Self {
        Self {
            values: crate::ast::stored_input::Parts::new(control),
            control,
        }
    }

    pub(super) fn take<T>(&mut self, value: impl Into<Produced<T>>) -> T {
        self.values.take(value.into())
    }

    pub(super) fn finish<T>(self, value: T) -> Result<Native<T>, InputError> {
        Native::new(self.values.finish(value)?, self.control)
    }
}
