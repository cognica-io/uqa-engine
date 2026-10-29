//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained descriptor sets share immutable independently admitted entries across prepared generations.

use super::{CompositeTypeDescriptor, Descriptors};
use crate::{
    expr::composites::{CompositeTypeCatalog, OwnedCompositeTypeDescriptor},
    ColumnType, SQLError,
};
use std::sync::Arc;
use uqa_core::memory::{OwnedMap, Produced, ProductionControl};

pub(crate) trait DescriptorLookup: CompositeTypeCatalog {
    fn lookup(&self, oid: u32) -> Option<&CompositeTypeDescriptor>;
}
impl DescriptorLookup for Descriptors {
    fn lookup(&self, oid: u32) -> Option<&CompositeTypeDescriptor> {
        self.get(&oid).map(AsRef::as_ref)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DescriptorSet(Option<Arc<Produced<OwnedMap<u32, OwnedCompositeTypeDescriptor>>>>);

impl DescriptorSet {
    pub(crate) fn owner(&self, oid: u32) -> Option<OwnedCompositeTypeDescriptor> {
        self.0.as_ref()?.get(&oid).cloned()
    }
    pub(crate) fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |entries| entries.len())
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(crate) fn get(&self, oid: u32) -> Option<&CompositeTypeDescriptor> {
        self.0.as_ref()?.get(&oid).map(std::ops::Deref::deref)
    }
    pub(crate) fn contains_key(&self, oid: u32) -> bool {
        self.get(oid).is_some()
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&u32, &CompositeTypeDescriptor)> {
        self.0
            .iter()
            .flat_map(|entries| entries.iter().map(|(oid, value)| (oid, &**value)))
    }
    pub(crate) fn from_entries(
        entries: OwnedMap<u32, OwnedCompositeTypeDescriptor>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if entries.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self(Some(
            control
                .finish(entries, control.empty_reservation())?
                .into_shared(control)?,
        )))
    }
    pub(crate) fn from_descriptors<'a>(
        descriptors: impl IntoIterator<Item = &'a CompositeTypeDescriptor>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut entries = OwnedMap::new();
        for descriptor in descriptors {
            insert(&mut entries, copy_descriptor(descriptor, control)?, control)?;
        }
        Self::from_entries(entries, control)
    }
}
impl PartialEq for DescriptorSet {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(oid, descriptor)| other.get(*oid) == Some(descriptor))
    }
}
impl Eq for DescriptorSet {}
impl DescriptorLookup for DescriptorSet {
    fn lookup(&self, oid: u32) -> Option<&CompositeTypeDescriptor> {
        self.get(oid)
    }
}
impl CompositeTypeCatalog for DescriptorSet {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(self.get(oid).map(|descriptor| Arc::new(descriptor.clone())))
    }
    fn composite_type_with_control(
        &self,
        oid: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<OwnedCompositeTypeDescriptor>, SQLError> {
        control.check()?;
        Ok(self
            .0
            .as_ref()
            .and_then(|entries| entries.get(&oid))
            .cloned())
    }
}

pub(crate) fn copy_descriptor(
    descriptor: &CompositeTypeDescriptor,
    control: &ProductionControl<'_>,
) -> Result<OwnedCompositeTypeDescriptor, SQLError> {
    OwnedCompositeTypeDescriptor::from_produced(
        CompositeTypeDescriptor::from_metadata_with_control(
            descriptor.type_oid,
            descriptor.relation_oid,
            descriptor.attributes.iter().map(|attribute| {
                Ok((
                    attribute.name.as_str(),
                    &attribute.ty,
                    attribute.number,
                    attribute.collation_oid,
                ))
            }),
            descriptor.dropped.iter().copied().map(Ok),
            control,
        )?,
        control,
    )
}

pub(crate) fn insert(
    entries: &mut OwnedMap<u32, OwnedCompositeTypeDescriptor>,
    descriptor: OwnedCompositeTypeDescriptor,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let Some(budget) = control.budget() {
        entries.insert_budgeted(descriptor.type_oid, descriptor, budget)?;
    } else {
        entries.insert(descriptor.type_oid, descriptor);
    }
    Ok(())
}

pub(crate) fn capture_type(
    ty: &ColumnType,
    catalog: Option<&dyn CompositeTypeCatalog>,
    entries: &mut OwnedMap<u32, OwnedCompositeTypeDescriptor>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match ty {
        ColumnType::Domain { base, .. } | ColumnType::Array(base) => {
            capture_type(base, catalog, entries, control)
        }
        ColumnType::Composite(reference) => {
            capture_descriptor(reference.oid, catalog, entries, control)
        }
        _ => Ok(()),
    }
}

pub(crate) fn capture_descriptor(
    oid: u32,
    catalog: Option<&dyn CompositeTypeCatalog>,
    entries: &mut OwnedMap<u32, OwnedCompositeTypeDescriptor>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if entries.contains_key(&oid) {
        return Ok(());
    }
    let descriptor = crate::expr::composites::descriptor_with_control(catalog, oid, control)?;
    insert(entries, descriptor.clone(), control)?;
    for attribute in &descriptor.attributes {
        capture_type(&attribute.ty, catalog, entries, control)?;
    }
    Ok(())
}
