//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared metadata stays with one prepared row producer and its original lease.

use std::sync::OnceLock;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RecordFieldType, RowDescriptor, ValueRetentionError,
};
use uqa_sql::{expr::EngineHook, type_resolution::OperandTypeScope, RowSchema, SQLError};

#[derive(Debug, Default)]
pub(crate) struct RowConstructorState {
    shared: OnceLock<Produced<RowDescriptor>>,
    known: ConstructorSlot<Option<Produced<RowDescriptor>>>,
    pub(super) parameter_dependent: bool,
}

impl RowConstructorState {
    pub(crate) fn reusable(
        &self,
        schema: Option<&RowSchema>,
        hook: Option<&dyn EngineHook>,
    ) -> Option<Option<RowDescriptor>> {
        Some(self.known.get(schema, hook)?.as_deref().cloned())
    }

    pub(crate) fn observe(&self) -> bool {
        !self.parameter_dependent && self.known.is_empty()
    }

    pub(crate) fn shared(&self, fields: &[RecordFieldType]) -> Option<RowDescriptor> {
        let known = self
            .known
            .0
            .get()
            .and_then(|(_, _, value)| value.as_deref());
        known
            .into_iter()
            .chain(self.shared.get().map(|value| &**value))
            .find(|saved| saved.field_types() == fields)
            .cloned()
    }

    pub(crate) fn remember(
        &self,
        descriptor: Option<&RowDescriptor>,
        schema: Option<&RowSchema>,
        scope: Option<OperandTypeScope>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if !self.known.is_empty() {
            return Ok(());
        }
        let Some(descriptor) = descriptor else {
            if let Some(scope) = scope {
                self.known.publish(None, schema, scope);
            }
            return Ok(());
        };
        let retained = if let Some(shared) = self
            .shared
            .get()
            .filter(|saved| saved.field_types() == descriptor.field_types())
        {
            if scope.is_none() {
                return Ok(());
            }
            control.finish((**shared).clone(), control.empty_reservation())?
        } else {
            if scope.is_none() && self.shared.get().is_some() {
                return Ok(());
            }
            match control.reserve(descriptor.retained_buffer_bytes()?) {
                Ok(memory) => control.finish(descriptor.clone(), memory)?,
                Err(ValueRetentionError::Memory(_)) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        };
        if let Some(scope) = scope {
            self.known.publish(Some(retained), schema, scope);
        } else {
            let _ = self.shared.set(retained);
        }
        Ok(())
    }
}

/// A successfully observed constructor type is valid only for the same immutable row schema and the catalog lookups that selected it.
#[derive(Debug)]
pub(in crate::scalar) struct ConstructorSlot<T>(OnceLock<(Option<RowSchema>, OperandTypeScope, T)>);

impl<T> Default for ConstructorSlot<T> {
    fn default() -> Self {
        Self(OnceLock::new())
    }
}

impl<T> ConstructorSlot<T> {
    pub(in crate::scalar) fn get(
        &self,
        schema: Option<&RowSchema>,
        hook: Option<&dyn EngineHook>,
    ) -> Option<&T> {
        let (saved, scope, value) = self.0.get()?;
        (super::same_schema(saved.as_ref(), schema) && scope.matches(hook)).then_some(value)
    }
    pub(in crate::scalar) fn is_empty(&self) -> bool {
        self.0.get().is_none()
    }
    pub(in crate::scalar) fn publish(
        &self,
        value: T,
        schema: Option<&RowSchema>,
        scope: OperandTypeScope,
    ) {
        let _ = self.0.set((schema.cloned(), scope, value));
    }
}

pub(in crate::scalar) type ArrayConstructorState =
    ConstructorSlot<Option<Produced<uqa_sql::ColumnType>>>;

#[cfg(test)]
thread_local! { pub(in crate::scalar) static CONSTRUCTOR_OBSERVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
