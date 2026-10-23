//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded whole-row presence bindings in the existing physical spill format.

use super::decode::BinaryReader;
use super::encode::{add_size, add_string_size, write_bytes, write_raw, write_u64};
use crate::spill::format::spill_error;
use crate::{ExecResult, RowSchema};
use std::io::Write;
use uqa_sql::ast::InternalColumnRef;

pub(super) const FLAG: u8 = 4;

pub(super) fn encoded_size(schema: &RowSchema) -> ExecResult<usize> {
    let mut bytes = 0;
    for (qualifier, _) in schema.whole_row_presences() {
        if bytes == 0 {
            bytes = 8;
        }
        add_string_size(&mut bytes, qualifier, "whole-row presence qualifier")?;
        add_size(&mut bytes, 16, "whole-row presence attribute")?;
    }
    Ok(bytes)
}

pub(super) fn encode(writer: &mut impl Write, schema: &RowSchema) -> ExecResult<()> {
    let mut bindings = schema.whole_row_presences().collect::<Vec<_>>();
    bindings.sort_unstable_by_key(|(qualifier, _)| *qualifier);
    write_u64(writer, bindings.len())?;
    for (qualifier, marker) in bindings {
        write_bytes(writer, qualifier.as_bytes())?;
        write_raw(
            writer,
            &marker.relation().raw().to_le_bytes(),
            "whole-row presence relation",
        )?;
        write_u64(writer, marker.attribute())?;
    }
    Ok(())
}

pub(super) fn decode(reader: &mut BinaryReader<'_>, schema: RowSchema) -> ExecResult<RowSchema> {
    let count = reader.read_count("whole-row presence count", 24)?;
    let mut bindings = Vec::new();
    bindings.try_reserve_exact(count).map_err(|error| {
        spill_error(format!(
            "cannot allocate whole-row presence bindings: {error}"
        ))
    })?;
    for _ in 0..count {
        let qualifier = reader.read_string("whole-row presence qualifier")?;
        let relation = reader.read_u64("whole-row presence relation")?;
        let attribute = u32::try_from(reader.read_u64("whole-row presence attribute")?)
            .map_err(|_| spill_error("whole-row presence attribute exceeds u32"))?;
        bindings.push((qualifier, InternalColumnRef::from_raw(relation, attribute)));
    }
    schema
        .with_whole_row_presences(bindings)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests;
