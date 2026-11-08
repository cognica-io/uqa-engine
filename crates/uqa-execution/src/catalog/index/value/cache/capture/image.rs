//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native index policy and exact evaluated values are independently streamable components.

use super::{CapturedValueIndex, CapturedValueIndexPolicy, CapturedValues, ValueIndexComparison};
use std::{io::Read, ops::Bound};
use uqa_core::{
    memory::{BudgetedString, MemoryError},
    value_image::{ValueImageError, ValueImageReader},
    DocId,
};
use uqa_storage::{
    image_io::{ImageInput, ImageRead, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

const MAGIC: &[u8; 8] = b"UQAEVI01";

/// Native policy preserves historical eligibility independently of the surviving value population. The enclosing importer must validate and supply exactly `len` distinct evaluated values before publication.
pub struct ValueIndexImageMetadata {
    pub field: BudgetedString,
    pub policy: CapturedValueIndexPolicy,
    pub len: usize,
}

impl ValueIndexImageMetadata {
    pub fn read_image(
        reader: &mut dyn Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut input = ImageInput { reader, control };
        if &input.array::<8>()? != MAGIC {
            return Err(invalid("unknown evaluated-index metadata image"));
        }
        let flags = input.array::<4>()?;
        if flags[..3].iter().any(|value| *value > 1) {
            return Err(invalid("invalid evaluated-index eligibility flag"));
        }
        let comparison = match flags[3] {
            0 => ValueIndexComparison::Ordered,
            1 => ValueIndexComparison::TypedField,
            2 => ValueIndexComparison::Fallible,
            _ => return Err(invalid("invalid evaluated-index comparison policy")),
        };
        let len = input.count()?;
        let field = BudgetedString::from_budgeted(input.text()?);
        input.end()?;
        control.check()?;
        Ok(Self {
            field,
            policy: CapturedValueIndexPolicy {
                carried: flags[0] != 0,
                has_temporal: flags[1] != 0,
                has_row_values: flags[2] != 0,
                comparison,
            },
            len,
        })
    }
}

impl CapturedValueIndex {
    pub fn metadata_image_reader<'a>(
        &'a self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.control.check()?;
        control.check()?;
        let mut header = [0; 28];
        header[..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&[
            u8::from(self.policy.carried),
            u8::from(self.policy.has_temporal),
            u8::from(self.policy.has_row_values),
            match self.policy.comparison {
                ValueIndexComparison::Ordered => 0,
                ValueIndexComparison::TypedField => 1,
                ValueIndexComparison::Fallible => 2,
            },
        ]);
        header[12..20].copy_from_slice(
            &u64::try_from(self.len()?)
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        );
        header[20..28].copy_from_slice(
            &u64::try_from(self.field.len())
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        );
        ImageReader::new(
            Metadata {
                header,
                field: self.field.as_bytes(),
                offset: 0,
            },
            control,
        )?
        .with_source_control(Some(&self.control), control)
    }

    /// Select one physical identity position, including a spilled deletion. A caller must account for deleted positions instead of scanning them inside a live-value step.
    pub fn next_image_position(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<(DocId, bool)>> {
        self.control.check()?;
        control.check()?;
        let result = match &self.values {
            CapturedValues::Resident(values) => Ok(values
                .first_from(after.as_ref().map_or(Bound::Unbounded, Bound::Excluded))
                .map(|(&id, _)| (id, true))),
            CapturedValues::Spilled(values) => values.next_position(after, control),
        }?;
        self.control.check()?;
        control.check()?;
        Ok(result)
    }

    /// Emit the original UQAVAL01 carrier without reevaluating its expression or decoding a spilled payload. The image mapping supplies its full-width document identity.
    pub fn value_image_reader<'a>(
        &'a self,
        id: DocId,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Option<ImageReader<'a>>> {
        self.control.check()?;
        control.check()?;
        let reader = match &self.values {
            CapturedValues::Resident(values) => values
                .get(id)
                .map(|value| {
                    ImageReader::new(
                        ResidentValue(ValueImageReader::new(
                            value,
                            control.memory(),
                            control.cancellation(),
                        )),
                        control,
                    )
                })
                .transpose()?,
            CapturedValues::Spilled(values) => values.value_image_reader(id, control)?,
        };
        reader
            .map(|reader| reader.with_source_control(Some(&self.control), control))
            .transpose()
    }
}

struct Metadata<'a> {
    header: [u8; 28],
    field: &'a [u8],
    offset: usize,
}

impl ImageRead for Metadata<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let bytes = if self.offset < self.header.len() {
            &self.header[self.offset..]
        } else {
            &self.field[self.offset - self.header.len()..]
        };
        let count = bytes.len().min(output.len());
        output[..count].copy_from_slice(&bytes[..count]);
        self.offset += count;
        Ok(count)
    }
}

struct ResidentValue<'a>(ValueImageReader<'a, 'a>);

impl ImageRead for ResidentValue<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.0.read(output).map_err(|error| match error {
            ValueImageError::Memory(error) => error.into(),
            ValueImageError::Cancelled(error) => error.into(),
            error => StorageBackendError::backend("evaluated-index value image", error),
        })
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
