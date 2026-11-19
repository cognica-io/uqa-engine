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

/// A selected composite descriptor or the native field producer pinned to its source catalog.
pub struct CompositeDescriptorInput<'a>(DescriptorInput<'a>);

enum DescriptorInput<'a> {
    Ready(Option<OwnedCompositeTypeDescriptor>),
    Native(crate::construction::Read<'a, Result<Option<OwnedCompositeTypeDescriptor>, SQLError>>),
}

impl<'a> CompositeDescriptorInput<'a> {
    pub fn ready(value: Option<OwnedCompositeTypeDescriptor>) -> Self {
        Self(DescriptorInput::Ready(value))
    }

    /// Retain just the selected native reader, admitted under the caller's operation.
    pub fn from_catalog<F>(
        control: &ProductionControl<'_>,
        create: impl FnOnce(crate::catalog::CatalogSteps) -> F,
    ) -> Result<Self, SQLError>
    where
        F: std::future::Future<Output = Result<Option<OwnedCompositeTypeDescriptor>, SQLError>>
            + Send
            + 'a,
    {
        Ok(Self(DescriptorInput::Native(
            crate::catalog::CatalogSteps::read(control, create)?,
        )))
    }

    pub(crate) async fn required<W: crate::catalog::CatalogWork + Send>(
        self,
        type_oid: u32,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedCompositeTypeDescriptor, SQLError> {
        let value = match self.0 {
            DescriptorInput::Ready(value) => value,
            DescriptorInput::Native(mut reader) => loop {
                if let Some(result) = reader.advance_window(control, 1)? {
                    break result;
                }
                work.checkpoint().await?;
            },
        };
        required(type_oid, value)
    }
}

fn source(
    catalog: Option<&dyn CompositeTypeCatalog>,
) -> Result<&dyn CompositeTypeCatalog, SQLError> {
    catalog.ok_or_else(|| SQLError::Internal("composite type catalog is unavailable".into()))
}

fn required(
    type_oid: u32,
    value: Option<OwnedCompositeTypeDescriptor>,
) -> Result<OwnedCompositeTypeDescriptor, SQLError> {
    value.ok_or_else(|| SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("type with OID {type_oid} does not exist"),
    })
}

pub(crate) fn begin_descriptor<'a>(
    catalog: Option<&'a dyn CompositeTypeCatalog>,
    type_oid: u32,
    control: &ProductionControl<'_>,
) -> Result<CompositeDescriptorInput<'a>, SQLError> {
    control.check()?;
    source(catalog)?.begin_composite_type(type_oid, control)
}

pub fn descriptor_with_control(
    catalog: Option<&dyn CompositeTypeCatalog>,
    type_oid: u32,
    control: &ProductionControl<'_>,
) -> Result<OwnedCompositeTypeDescriptor, SQLError> {
    control.check()?;
    required(
        type_oid,
        source(catalog)?.composite_type_with_control(type_oid, control)?,
    )
}

#[cfg(test)]
mod tests;
