//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native image construction transfers decoded definitions and their original admission.

use super::{Budgeted, MemoryBudget, MemoryError, TableDefinition, TableDefinitionValue};
use crate::{
    ast::{ColumnDef, TableCheck, TableHierarchy},
    catalog::image,
    SQLError,
};
use std::io::Read;
use uqa_core::CancellationToken;

impl<T: TableDefinitionValue> TableDefinition<T> {
    fn from_image_parts(
        value: Budgeted<T>,
        children: usize,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        cancellation.check()?;
        let value = value.into_shared()?;
        let bytes = value
            .reserved_bytes()
            .checked_add(children)
            .ok_or(MemoryError::SizeOverflow)?;
        cancellation.check()?;
        Ok(Self { value, bytes })
    }
}

macro_rules! definition_image {
    ($value:ty, $read:ident) => {
        impl TableDefinition<$value> {
            /// Decode directly into an unpublished native generation. The existing image owner validates the complete representation; the decoded payload, literal leases and cached footprint transfer without a second copy or expression evaluation. Relation-wide validation remains with the enclosing restore owner.
            pub fn read_image(
                reader: &mut dyn Read,
                memory: &MemoryBudget,
                cancellation: &CancellationToken,
            ) -> Result<Self, SQLError> {
                let (value, children) = image::$read(reader, memory, cancellation)?;
                Self::from_image_parts(value, children, cancellation)
            }
        }
    };
}

definition_image!(Vec<ColumnDef>, read_columns_image_with_footprint);
definition_image!(Vec<TableCheck>, read_table_checks_image_with_footprint);
definition_image!(TableHierarchy, read_table_hierarchy_image_with_footprint);
