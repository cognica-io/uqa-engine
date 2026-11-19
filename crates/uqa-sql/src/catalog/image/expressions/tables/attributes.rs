//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dropped physical slots retain original layout independently of former type definitions.

use super::{DefinitionImageReader, Frame, Part};
use crate::{catalog::relation_attributes::DroppedAttribute, SQLError};
use uqa_core::{memory::MemoryBudget, CancellationToken};

/// Borrow ordered dropped attributes without synthesizing types, names or physical positions.
pub fn dropped_attributes_image_reader<'a>(
    values: &'a [DroppedAttribute],
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
) -> Result<DefinitionImageReader<'a>, SQLError> {
    DefinitionImageReader::start(
        Frame::Table(Part::Dropped(values)),
        *b"UQADAT01",
        values.len(),
        memory,
        cancellation,
    )
}

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn dropped_attributes(
        &mut self,
        values: &'a [DroppedAttribute],
    ) -> Result<(), SQLError> {
        if let Some((first, rest)) = values.split_first() {
            let DroppedAttribute {
                number,
                type_length,
                type_modifier,
                dimensions,
                by_value,
                alignment,
                storage,
                collation,
                identity,
                is_local,
                inheritance_count,
            } = first;
            self.fields.scalar(number.to_le_bytes());
            for value in [type_length, type_modifier, dimensions] {
                self.fields.scalar(value.to_le_bytes());
            }
            self.fields.flag(*by_value);
            self.fields.text(alignment);
            self.fields.text(storage);
            self.fields.scalar(collation.to_le_bytes());
            self.fields.text(identity);
            self.fields.flag(*is_local);
            self.fields.scalar(inheritance_count.to_le_bytes());
            self.table_push(Part::Dropped(rest))?;
        }
        Ok(())
    }
}
