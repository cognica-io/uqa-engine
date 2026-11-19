//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict metadata images move their decoded payload and admission into one shared owner.

use super::TableMetadata;
use crate::{ast, catalog::image, catalog::relation_attributes::DroppedAttribute, SQLError};
use std::io::Read;
use uqa_core::{image_io::ImageDecoder, memory::MemoryBudget, CancellationToken};

macro_rules! metadata_image {
    ($value:ty, $read:path, $resume:path) => {
        impl TableMetadata<$value> {
            /// Read an unpublished native metadata generation without copying its population. Existing strict image validation precedes publication; relation-wide validation belongs to the enclosing restore owner.
            pub fn read_image(
                reader: &mut dyn Read,
                memory: &MemoryBudget,
                cancellation: &CancellationToken,
            ) -> Result<Self, SQLError> {
                let value = $read(reader, memory, cancellation)?;
                Self::from_admitted(value, cancellation)
            }

            /// Retain private native fields through the enclosing byte/poll window. Only a complete strict image transfers into the shared metadata owner.
            pub fn image_input(
                memory: &MemoryBudget,
                cancellation: &CancellationToken,
            ) -> Result<ImageDecoder<Self, SQLError>, SQLError> {
                Ok(ImageDecoder::new(
                    memory,
                    cancellation,
                    |mut reader| async move {
                        let input = reader.input();
                        let cancellation = input.cancellation;
                        let value = $resume(input).await?;
                        Self::from_admitted(value, cancellation)
                    },
                )?)
            }
        }
    };
}

metadata_image!(
    Vec<ast::ForeignKey>,
    image::read_foreign_keys_image,
    image::read_foreign_keys_image_from
);
metadata_image!(
    Vec<ast::TableKeyConstraint>,
    image::read_table_keys_image,
    image::read_table_keys_image_from
);
metadata_image!(
    Vec<DroppedAttribute>,
    image::read_dropped_attributes_image,
    image::read_dropped_attributes_image_from
);

metadata_image!(
    crate::catalog::security::BoundTableSecurity,
    crate::catalog::security::BoundTableSecurity::read_image,
    crate::catalog::security::BoundTableSecurity::read_image_from
);
