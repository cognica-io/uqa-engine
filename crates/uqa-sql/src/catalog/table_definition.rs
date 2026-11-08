//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table expression publication owns evaluated literals before exposing an immutable generation.

use crate::SQLError;
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, MemoryError, Produced, ProductionControl},
    CancellationToken,
};

mod image;

mod sealed {
    pub trait Definition {}
}

pub trait TableDefinitionValue: sealed::Definition + Clone + Send + Sync + 'static {
    fn produce(&self, control: &ProductionControl<'_>)
        -> Result<(Produced<Self>, usize), SQLError>;
}

macro_rules! definition {
    ($($ty:ty),* $(,)?) => {$ (
        impl sealed::Definition for $ty {}
        impl TableDefinitionValue for $ty {
            fn produce(&self, control: &ProductionControl<'_>) -> Result<(Produced<Self>, usize), SQLError> {
                crate::plan::copy::capture_stored_with_footprint(self, control)
            }
        }
    )*};
}
definition!(
    Vec<crate::ast::ColumnDef>,
    Vec<crate::ast::TableCheck>,
    crate::ast::TableHierarchy
);

/// Independently constructed native definitions, including resident evaluated literals and their constructor reservations. Copying or capturing the handle never reads a former query source.
#[derive(Clone, Debug)]
pub struct TableDefinition<T: TableDefinitionValue> {
    value: Arc<Budgeted<T>>,
    bytes: usize,
}

impl<T: TableDefinitionValue> TableDefinition<T> {
    /// Native catalog publication uses the same independent allocation domain as the named definition registries. Construction is fallible; callers publish only the completed value inside their existing definition transaction.
    pub fn new(value: &T) -> Result<Self, SQLError> {
        let memory = MemoryBudget::new(usize::MAX);
        let cancellation = CancellationToken::new();
        Self::with_control(
            value,
            &ProductionControl::new(&memory, &cancellation, &cancellation),
        )
    }

    pub fn with_control(value: &T, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        control.check()?;
        if control.budget().is_none() {
            return Err(SQLError::Internal(
                "table definition publication requires an admitted control".into(),
            ));
        }
        let (value, children) = value.produce(control)?;
        let value = value
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("admitted table definition"));
        let value = value.into_shared()?;
        let bytes = value
            .reserved_bytes()
            .checked_add(children)
            .ok_or(MemoryError::SizeOverflow)?;
        control.check()?;
        Ok(Self { value, bytes })
    }

    pub fn capture_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let memory = control.reserve(self.bytes)?;
        Ok(control.finish(self.clone(), memory)?)
    }

    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }

    pub fn shares_definition(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.value, &other.value)
    }

    pub fn shared(&self) -> SharedTableDefinition<T> {
        SharedTableDefinition::Native(self.clone())
    }

    pub fn write<G: DerefMut<Target = Self>>(owner: G) -> TableDefinitionWrite<T, G> {
        TableDefinitionWrite { owner }
    }
}

impl<T: TableDefinitionValue> Deref for TableDefinition<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

/// Borrowed consumers preserve either a native publication owner or an existing immutable external catalog generation. The external variant never becomes a native backup source implicitly.
#[derive(Clone, Debug)]
pub enum SharedTableDefinition<T: TableDefinitionValue> {
    Native(TableDefinition<T>),
    External(Arc<T>),
}

impl<T: TableDefinitionValue> From<Arc<T>> for SharedTableDefinition<T> {
    fn from(value: Arc<T>) -> Self {
        Self::External(value)
    }
}

impl<T: TableDefinitionValue> From<T> for SharedTableDefinition<T> {
    fn from(value: T) -> Self {
        Self::External(Arc::new(value))
    }
}

impl<T: TableDefinitionValue + Default> Default for SharedTableDefinition<T> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<T: TableDefinitionValue> Deref for SharedTableDefinition<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self {
            Self::Native(value) => value,
            Self::External(value) => value,
        }
    }
}

impl<T: TableDefinitionValue> AsRef<T> for SharedTableDefinition<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: TableDefinitionValue> SharedTableDefinition<T> {
    pub fn shares_definition(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Native(left), Self::Native(right)) => left.shares_definition(right),
            (Self::External(left), Self::External(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

/// A publication guard never exposes mutable references into its native generation. Construction completes before replacement, so failure and unwinding preserve the previous definition and its footprint.
pub struct TableDefinitionWrite<T: TableDefinitionValue, G: DerefMut<Target = TableDefinition<T>>> {
    owner: G,
}

impl<T: TableDefinitionValue, G: DerefMut<Target = TableDefinition<T>>> TableDefinitionWrite<T, G> {
    pub fn publish(&mut self, value: T) -> Result<(), SQLError> {
        let value = TableDefinition::new(&value)?;
        *self.owner = value;
        Ok(())
    }

    pub fn edit<R>(
        &mut self,
        edit: impl FnOnce(&mut T) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        let mut candidate = (**self).clone();
        let result = edit(&mut candidate)?;
        self.publish(candidate)?;
        Ok(result)
    }
}

impl<T: TableDefinitionValue, G: DerefMut<Target = TableDefinition<T>>> Deref
    for TableDefinitionWrite<T, G>
{
    type Target = T;
    fn deref(&self) -> &T {
        &self.owner
    }
}

impl<T: TableDefinitionValue + serde::Serialize> serde::Serialize for SharedTableDefinition<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (**self).serialize(serializer)
    }
}

impl<T: TableDefinitionValue> serde::Serialize for TableDefinition<T>
where
    T: serde::Serialize,
{
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (**self).serialize(serializer)
    }
}

#[cfg(test)]
mod tests;
