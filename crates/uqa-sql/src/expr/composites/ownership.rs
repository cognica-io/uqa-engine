//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A descriptor's admitted payload and its lease share one immutable lifetime.

use super::{CompositeTypeCatalog, CompositeTypeDescriptor};
use crate::SQLError;
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Debug, Clone)]
enum DescriptorOwner {
    Retained(Arc<CompositeTypeDescriptor>),
    Produced(Arc<Produced<CompositeTypeDescriptor>>),
}

/// A descriptor read shares either an existing immutable catalog generation or the newly admitted producer result. A produced descriptor cannot be extracted as a raw Arc that would discard its lease.
#[derive(Debug, Clone)]
pub struct OwnedCompositeTypeDescriptor(DescriptorOwner);

impl OwnedCompositeTypeDescriptor {
    /// Share metadata that an existing catalog owner already retains, without copying its attributes.
    pub fn retained(descriptor: Arc<CompositeTypeDescriptor>) -> Self {
        Self(DescriptorOwner::Retained(descriptor))
    }

    pub fn from_produced(
        descriptor: Produced<CompositeTypeDescriptor>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self(DescriptorOwner::Produced(
            descriptor.into_shared(control)?,
        )))
    }

    /// The new payload charged to this producer; preexisting immutable catalog metadata is retained by its original owner.
    pub fn reserved_bytes(&self) -> usize {
        match &self.0 {
            DescriptorOwner::Retained(_) => 0,
            DescriptorOwner::Produced(descriptor) => descriptor.reserved_bytes(),
        }
    }
}

impl std::ops::Deref for OwnedCompositeTypeDescriptor {
    type Target = CompositeTypeDescriptor;
    fn deref(&self) -> &Self::Target {
        match &self.0 {
            DescriptorOwner::Retained(descriptor) => descriptor,
            DescriptorOwner::Produced(descriptor) => descriptor,
        }
    }
}

pub fn descriptor_with_control(
    catalog: Option<&dyn CompositeTypeCatalog>,
    type_oid: u32,
    control: &ProductionControl<'_>,
) -> Result<OwnedCompositeTypeDescriptor, SQLError> {
    control.check()?;
    let catalog = catalog
        .ok_or_else(|| SQLError::Internal("composite type catalog is unavailable".into()))?;
    catalog
        .composite_type_with_control(type_oid, control)?
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type with OID {type_oid} does not exist"),
        })
}

#[cfg(test)]
mod tests;
