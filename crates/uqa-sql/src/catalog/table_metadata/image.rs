//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict metadata images move their decoded payload and admission into one shared owner.

use super::TableMetadata;
use crate::{ast, catalog::image, catalog::relation_attributes::DroppedAttribute, SQLError};
use std::io::Read;
use uqa_core::{memory::MemoryBudget, CancellationToken};

macro_rules! metadata_image {
    ($value:ty, $read:ident) => {
        impl TableMetadata<$value> {
            /// Read an unpublished native metadata generation without copying its population. Existing strict image validation precedes publication; relation-wide validation belongs to the enclosing restore owner.
            pub fn read_image(
                reader: &mut dyn Read,
                memory: &MemoryBudget,
                cancellation: &CancellationToken,
            ) -> Result<Self, SQLError> {
                let value = image::$read(reader, memory, cancellation)?;
                Self::from_admitted(value, cancellation)
            }
        }
    };
}

metadata_image!(Vec<ast::ForeignKey>, read_foreign_keys_image);
metadata_image!(Vec<ast::TableKeyConstraint>, read_table_keys_image);
metadata_image!(Vec<DroppedAttribute>, read_dropped_attributes_image);
