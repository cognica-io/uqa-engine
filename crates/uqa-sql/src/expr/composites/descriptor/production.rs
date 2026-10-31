//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CompositeAttribute, CompositeTypeDescriptor};
use crate::{ast::ColumnType, expr::composites::DroppedCompositeAttribute, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

impl CompositeTypeDescriptor {
    /// Build the selected descriptor directly from borrowed catalog fields. Dropped physical positions precede live attributes, preserving the ordinary producer's diagnostic order.
    pub fn from_metadata_with_control<'a>(
        type_oid: u32,
        relation_oid: u32,
        attributes: impl IntoIterator<
            Item = Result<(&'a str, &'a ColumnType, i16, Option<u32>), SQLError>,
        >,
        dropped: impl IntoIterator<Item = Result<DroppedCompositeAttribute, SQLError>>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        control.check()?;
        let mut removed = ProductionVec::new(*control);
        for attribute in dropped {
            control.check()?;
            removed.push_copy(attribute?)?;
        }
        let mut fields = ProductionVec::new(*control);
        for attribute in attributes {
            control.check()?;
            let (name, ty, number, collation_oid) = attribute?;
            let (name, name_memory) = control.copy_text(name)?.into_parts();
            let (ty, type_memory) = ty.clone_with_control(control)?.into_parts();
            fields.push_produced(control.finish(
                CompositeAttribute {
                    name,
                    ty,
                    collation_oid,
                    number,
                },
                control.combine(name_memory, type_memory),
            )?)?;
        }
        let (dropped, dropped_memory) = removed.finish()?.into_parts();
        let (attributes, memory) = fields.finish()?.into_parts();
        Ok(control.finish(
            Self {
                dropped,
                type_oid,
                relation_oid,
                attributes,
            },
            control.combine(memory, dropped_memory),
        )?)
    }
}
