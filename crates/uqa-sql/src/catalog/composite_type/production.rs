//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::StoredComposite;
use crate::{
    expr::composites::{CompositeTypeDescriptor, DroppedCompositeAttribute},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

impl<Owner> StoredComposite<Owner> {
    pub fn descriptor_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CompositeTypeDescriptor>, SQLError> {
        CompositeTypeDescriptor::from_metadata_with_control(
            self.oid,
            self.relation_oid,
            self.live_attributes().map(|attribute| {
                Ok((
                    attribute.name.as_str(),
                    &attribute.ty,
                    attribute.number,
                    Some(attribute.collation_oid()),
                ))
            }),
            self.attributes
                .iter()
                .filter(|attribute| attribute.dropped)
                .map(|attribute| {
                    Ok(DroppedCompositeAttribute::from_type(
                        attribute.number,
                        &attribute.ty,
                    ))
                }),
            control,
        )
    }
}
