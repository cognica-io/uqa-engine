//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable table names and constraints retain publication-time payload capacities.

use crate::SQLError;
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};
use uqa_core::memory::{MemoryError, Produced, ProductionControl};

mod footprint;
mod image;
mod shared;
pub use shared::SharedTableMetadata;
#[cfg(test)]
mod tests;

/// Metadata with exclusively owned, non-executable payloads. SQL seals this set so shared execution sources cannot enter an immutable generation without their own capture boundary.
pub trait TableMetadataValue: footprint::Payload + Clone + Send + Sync + 'static {}
impl TableMetadataValue for Vec<String> {}
impl TableMetadataValue for Option<String> {}
impl TableMetadataValue for Vec<crate::ast::ForeignKey> {}
impl TableMetadataValue for Vec<crate::ast::TableKeyConstraint> {}
impl TableMetadataValue for Vec<crate::catalog::relation_attributes::DroppedAttribute> {}

/// One immutable native metadata generation. Capture admits the original capacities without visiting or copying its population; mutation detaches before changing a shared generation.
#[derive(Debug, Clone)]
pub struct TableMetadata<T: TableMetadataValue> {
    value: SharedTableMetadata<T>,
    bytes: Option<usize>,
}

impl<T: TableMetadataValue> TableMetadata<T> {
    pub fn new(value: T) -> Self {
        Self::from_snapshot(Arc::new(value))
    }

    pub fn from_snapshot(value: Arc<T>) -> Self {
        let bytes = footprint::retained(&*value);
        Self {
            value: value.into(),
            bytes,
        }
    }

    pub fn snapshot(&self) -> SharedTableMetadata<T> {
        self.value.clone()
    }

    pub fn capture_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        control.check()?;
        let memory = control.reserve(self.bytes.ok_or(MemoryError::SizeOverflow)?)?;
        Ok(control.finish(self.clone(), memory)?)
    }

    /// Keep the owning lock or borrow through footprint publication, including unwinding. Callers cannot expose a modified generation with its predecessor's capacities.
    pub fn write<G: DerefMut<Target = Self>>(owner: G) -> TableMetadataWrite<T, G> {
        TableMetadataWrite {
            owner,
            changed: false,
        }
    }
}

impl<T: TableMetadataValue> Deref for TableMetadata<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: TableMetadataValue + serde::Serialize> serde::Serialize for TableMetadata<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (**self).serialize(serializer)
    }
}

pub struct TableMetadataWrite<T: TableMetadataValue, G: DerefMut<Target = TableMetadata<T>>> {
    owner: G,
    changed: bool,
}

impl<T: TableMetadataValue, G: DerefMut<Target = TableMetadata<T>>> Deref
    for TableMetadataWrite<T, G>
{
    type Target = T;
    fn deref(&self) -> &T {
        &self.owner.value
    }
}

impl<T: TableMetadataValue, G: DerefMut<Target = TableMetadata<T>>> DerefMut
    for TableMetadataWrite<T, G>
{
    fn deref_mut(&mut self) -> &mut T {
        self.changed = true;
        self.owner.bytes = None;
        self.owner.value.make_mut()
    }
}

impl<T: TableMetadataValue, G: DerefMut<Target = TableMetadata<T>>> Drop
    for TableMetadataWrite<T, G>
{
    fn drop(&mut self) {
        if self.changed {
            self.owner.bytes = self.owner.value.retained_bytes();
        }
    }
}
